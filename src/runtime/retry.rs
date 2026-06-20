//! Queue driven retry (DESIGN §7, roadmap step 7b): poll an offline peer's onion
//! only while messages are queued for them, never to probe presence. Jittered
//! widening backoff, one dial per tick, quiet on failure. Full rationale in
//! `specs/20260517-implementation-notes.md`.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use arti_client::TorClient;
use rand::Rng;
use safelog::DisplayRedacted;
use tokio::sync::{mpsc, Mutex};
use tokio::time::{interval, Instant, MissedTickBehavior};
use tor_hscrypto::pk::HsId;
use tor_rtcompat::PreferredRuntime;

use crate::identity::PeerId;
use crate::noise::StaticKey;

use super::events::{AppMsg, ContactRoute};
use super::{hex32, Connections, SharedQueue, SharedRoutes};

const BASE_MIN: Duration = Duration::from_secs(45);
const BASE_MAX: Duration = Duration::from_secs(90);
const MAX_SHIFT: u32 = 5; // window doubles per failure up to 2^MAX_SHIFT
const TICK: Duration = Duration::from_secs(5);

/// Random delay in the widening, capped backoff window. `attempt` is 1-based.
fn backoff<R: Rng>(attempt: u32, rng: &mut R) -> Duration {
    let shift = attempt.saturating_sub(1).min(MAX_SHIFT);
    let factor = 1u64 << shift;
    let lo = BASE_MIN.as_millis() as u64 * factor;
    let hi = BASE_MAX.as_millis() as u64 * factor;
    Duration::from_millis(rng.gen_range(lo..=hi))
}

/// Rebuild a dialable `.onion` from a contact's stored v3 onion key bytes.
fn onion_address(hs_id: &[u8; 32]) -> String {
    let id: HsId = (*hs_id).into();
    format!("{}", id.display_unredacted())
}

/// The queue driven gate: routes that currently have a queued backlog, and only
/// those. A verified contact with nothing queued is never dialed.
fn candidates(routes: &[ContactRoute], pending: &HashSet<String>) -> Vec<ContactRoute> {
    routes
        .iter()
        .filter(|r| pending.contains(&hex32(&r.remote_static)))
        .cloned()
        .collect()
}

/// Per peer poll schedule: next eligible time and consecutive failure count.
#[derive(Default)]
struct Schedule {
    next_due: HashMap<[u8; 32], Instant>,
    failures: HashMap<[u8; 32], u32>,
}

impl Schedule {
    /// Drop peers no longer queued; reset failures for any that reconnected.
    fn reconcile(&mut self, candidates: &[ContactRoute], connected: &HashSet<[u8; 32]>) {
        let known: HashSet<[u8; 32]> = candidates.iter().map(|r| r.remote_static).collect();
        self.next_due.retain(|k, _| known.contains(k));
        self.failures.retain(|k, _| known.contains(k));
        for k in connected {
            if known.contains(k) {
                self.failures.insert(*k, 0);
            }
        }
    }

    /// Make every peer due now, keeping failure counts so backoff is not lost.
    fn kick(&mut self) {
        self.next_due.clear();
    }

    /// Pick the most overdue eligible peer and push its next attempt out.
    fn take_due<R: Rng>(
        &mut self,
        candidates: &[ContactRoute],
        connected: &HashSet<[u8; 32]>,
        in_flight: &HashSet<[u8; 32]>,
        now: Instant,
        rng: &mut R,
    ) -> Option<ContactRoute> {
        let pick = candidates
            .iter()
            .filter(|r| !connected.contains(&r.remote_static))
            .filter(|r| !in_flight.contains(&r.remote_static))
            .filter(|r| {
                self.next_due
                    .get(&r.remote_static)
                    .is_none_or(|due| *due <= now)
            })
            // unscheduled peers sort as None, before any Some, so they go first
            .min_by_key(|r| self.next_due.get(&r.remote_static).copied())
            .cloned()?;

        let attempt = {
            let f = self.failures.entry(pick.remote_static).or_insert(0);
            *f = f.saturating_add(1);
            *f
        };
        self.next_due
            .insert(pick.remote_static, now + backoff(attempt, rng));
        Some(pick)
    }
}

/// Spawn the retry loop. Runs only after Tor is up, since it polls onions.
#[allow(clippy::too_many_arguments)]
pub fn spawn(
    tor_client: TorClient<PreferredRuntime>,
    routes: SharedRoutes,
    mut kick_rx: mpsc::Receiver<()>,
    static_key: Arc<StaticKey>,
    own_id: PeerId,
    msg_tx: mpsc::Sender<AppMsg>,
    connections: Connections,
    queue: SharedQueue,
) {
    tokio::spawn(async move {
        let mut sched = Schedule::default();
        let in_flight: Arc<Mutex<HashSet<[u8; 32]>>> = Arc::new(Mutex::new(HashSet::new()));
        let mut ticker = interval(TICK);
        ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);

        loop {
            tokio::select! {
                _ = ticker.tick() => {}
                recv = kick_rx.recv() => match recv {
                    Some(()) => sched.kick(),
                    None => break,
                },
            }

            // queue driven candidate set; a locked or absent vault yields nothing
            let pending: HashSet<String> = {
                let guard = queue.lock().await;
                match guard.as_ref() {
                    Some(q) => q.contacts_with_pending().unwrap_or_default().into_iter().collect(),
                    None => HashSet::new(),
                }
            };
            if pending.is_empty() {
                continue;
            }
            let candidates = {
                let guard = routes.lock().await;
                candidates(&guard, &pending)
            };

            let now = Instant::now();
            let connected: HashSet<[u8; 32]> =
                connections.lock().await.keys().copied().collect();
            sched.reconcile(&candidates, &connected);
            let busy = in_flight.lock().await.clone();

            let pick = {
                let mut rng = rand::thread_rng();
                sched.take_due(&candidates, &connected, &busy, now, &mut rng)
            };

            if let Some(route) = pick {
                in_flight.lock().await.insert(route.remote_static);
                let onion = onion_address(&route.hs_id);
                let client = tor_client.clone();
                let static_key = static_key.clone();
                let msg_tx = msg_tx.clone();
                let connections = connections.clone();
                let queue = queue.clone();
                let in_flight = in_flight.clone();
                let remote_static = route.remote_static;
                tokio::spawn(async move {
                    // quiet = true: a failed poll stays invisible to the user
                    super::connect_onion_peer(
                        client,
                        onion,
                        static_key,
                        own_id,
                        msg_tx,
                        connections,
                        queue,
                        true,
                    )
                    .await;
                    in_flight.lock().await.remove(&remote_static);
                });
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn route(tag: u8) -> ContactRoute {
        ContactRoute {
            remote_static: [tag; 32],
            hs_id: [tag; 32],
            label: format!("peer-{tag}"),
        }
    }

    #[test]
    fn only_peers_with_a_backlog_are_candidates() {
        let routes = vec![route(1), route(2)];
        let pending: HashSet<String> = [hex32(&[1u8; 32])].into_iter().collect();
        let got = candidates(&routes, &pending);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].remote_static, [1u8; 32]);
    }

    #[test]
    fn no_backlog_means_no_candidate_so_no_presence_probing() {
        let routes = vec![route(1), route(2), route(3)];
        assert!(candidates(&routes, &HashSet::new()).is_empty());
    }

    #[test]
    fn backoff_first_attempt_is_in_base_window() {
        let mut rng = rand::thread_rng();
        for _ in 0..1000 {
            let d = backoff(1, &mut rng);
            assert!(
                d >= BASE_MIN && d <= BASE_MAX,
                "first poll {d:?} outside [{BASE_MIN:?}, {BASE_MAX:?}]"
            );
        }
    }

    #[test]
    fn backoff_widens_with_failures_and_caps() {
        let mut rng = rand::thread_rng();
        for attempt in 1..=8u32 {
            let shift = attempt.saturating_sub(1).min(MAX_SHIFT);
            let factor = 1u32 << shift;
            let lo = BASE_MIN * factor;
            let hi = BASE_MAX * factor;
            for _ in 0..200 {
                let d = backoff(attempt, &mut rng);
                assert!(d >= lo && d <= hi, "attempt {attempt}: {d:?} not in [{lo:?},{hi:?}]");
            }
        }
        let capped_lo = BASE_MIN * (1u32 << MAX_SHIFT);
        let capped_hi = BASE_MAX * (1u32 << MAX_SHIFT);
        for _ in 0..200 {
            let d = backoff(99, &mut rng);
            assert!(d >= capped_lo && d <= capped_hi, "past cap: {d:?}");
        }
    }

    #[test]
    fn take_due_skips_connected_and_in_flight() {
        let mut sched = Schedule::default();
        let candidates = vec![route(1), route(2), route(3)];
        let now = Instant::now();
        let mut rng = rand::thread_rng();

        let connected: HashSet<_> = [[1u8; 32]].into_iter().collect();
        let busy: HashSet<_> = [[2u8; 32]].into_iter().collect();
        let pick = sched.take_due(&candidates, &connected, &busy, now, &mut rng);
        assert_eq!(pick.map(|r| r.remote_static), Some([3u8; 32]));
    }

    #[test]
    fn take_due_dials_one_per_call_then_holds_off() {
        let mut sched = Schedule::default();
        let candidates = vec![route(1), route(2)];
        let empty = HashSet::new();
        let now = Instant::now();
        let mut rng = rand::thread_rng();

        let first = sched.take_due(&candidates, &empty, &empty, now, &mut rng).unwrap();
        let second = sched.take_due(&candidates, &empty, &empty, now, &mut rng).unwrap();
        assert_ne!(first.remote_static, second.remote_static);
        assert!(sched.take_due(&candidates, &empty, &empty, now, &mut rng).is_none());
    }

    #[test]
    fn reconcile_forgets_gone_and_resets_connected_failures() {
        let mut sched = Schedule::default();
        let now = Instant::now();
        let mut rng = rand::thread_rng();

        let candidates = vec![route(1)];
        let empty = HashSet::new();
        sched.take_due(&candidates, &empty, &empty, now, &mut rng);
        sched.kick();
        sched.take_due(&candidates, &empty, &empty, now, &mut rng);
        assert_eq!(sched.failures.get(&[1u8; 32]).copied(), Some(2));

        let connected: HashSet<_> = [[1u8; 32]].into_iter().collect();
        sched.reconcile(&candidates, &connected);
        assert_eq!(sched.failures.get(&[1u8; 32]).copied(), Some(0));

        sched.reconcile(&[], &HashSet::new());
        assert!(sched.failures.is_empty());
        assert!(sched.next_due.is_empty());
    }

    #[test]
    fn kick_makes_everything_due_without_losing_failures() {
        let mut sched = Schedule::default();
        let candidates = vec![route(1)];
        let empty = HashSet::new();
        let now = Instant::now();
        let mut rng = rand::thread_rng();

        sched.take_due(&candidates, &empty, &empty, now, &mut rng).unwrap();
        assert!(sched.take_due(&candidates, &empty, &empty, now, &mut rng).is_none());

        sched.kick();
        let again = sched.take_due(&candidates, &empty, &empty, now, &mut rng);
        assert_eq!(again.map(|r| r.remote_static), Some([1u8; 32]));
        assert_eq!(sched.failures.get(&[1u8; 32]).copied(), Some(2));
    }
}
