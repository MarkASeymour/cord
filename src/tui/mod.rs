use std::collections::{HashMap, HashSet, VecDeque};
use std::io::{self, Stdout};
use std::net::SocketAddr;
use std::time::Duration;

use crossterm::event::EventStream;
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use futures::StreamExt;
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use tokio::sync::mpsc;
use tokio::time::interval;

use safelog::DisplayRedacted;
use tor_hscrypto::pk::HsId;

use crate::contacts::{self, Contact, ContactBlob, ContactStatus};
use crate::discovery::KnownPeer;
use crate::errors::CordError;
use crate::identity::{Identity, PeerId};
use crate::runtime::events::{AppMsg, ContactRoute, DeliveryStatus, LocalAddrs, TransportCmd};

pub mod input;
pub mod layout;
pub mod view;

const CHAT_LOG_CAP: usize = 500;
const SYSTEM_LOG_CAP: usize = 500;

pub struct App {
    pub identity: Identity,
    pub transport_state: TransportState,
    pub peers: HashMap<PeerId, KnownPeer>,
    pub contacts: Vec<Contact>,
    pub contacts_dirty: bool,
    pub chat_log: VecDeque<ChatEntry>,
    pub system_log: VecDeque<String>,
    pub input_buffer: String,
    pub mode: InputMode,
    pub vault_ready: bool,
    pub vault_locked: bool,
    pub connected: HashSet<[u8; 32]>,
    pub focus: Pane,
    pub chat_view: PaneView,
    pub log_view: PaneView,
    pub should_quit: bool,
}

pub enum TransportState {
    Bootstrapping,
    BootstrappingTor {
        percent: u8,
        summary: String,
        lan: Option<SocketAddr>,
    },
    Lan {
        listening_on: SocketAddr,
    },
    Onion {
        onion_name: String,
        hs_id: [u8; 32],
        lan: Option<SocketAddr>,
    },
    Failed(String),
}

pub enum ChatEntry {
    Incoming { from: String, text: String },
    Outgoing {
        to: String,
        text: String,
        id: u64,
        status: DeliveryStatus,
    },
}

/// The two scrollable panes.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Pane {
    Conversation,
    SystemLog,
}

/// Scroll state for one pane. `offset` is the absolute row the pane is scrolled
/// to; `None` means follow the newest line (pinned to the bottom). `max` and
/// `page` are cached from the last render so the input handler can page and
/// clamp without recomputing the wrapped layout. Holding an absolute offset
/// (rather than a distance from the bottom) keeps the view anchored to the same
/// lines as new content arrives below.
pub struct PaneView {
    pub offset: Option<u16>,
    pub max: u16,
    pub page: u16,
}

impl PaneView {
    fn new() -> Self {
        Self {
            offset: None,
            max: 0,
            page: 1,
        }
    }

    /// Row to scroll to this frame, given the freshly computed maximum.
    pub fn resolve(&self, max: u16) -> u16 {
        match self.offset {
            None => max,
            Some(y) => y.min(max),
        }
    }

    /// True when the pane is held above the newest line.
    pub fn is_scrolled(&self) -> bool {
        self.offset.is_some_and(|y| y < self.max)
    }

    pub fn page_up(&mut self) {
        let cur = self.offset.unwrap_or(self.max);
        self.offset = Some(cur.saturating_sub(self.page.max(1)));
    }

    pub fn page_down(&mut self) {
        let cur = self.offset.unwrap_or(self.max);
        let next = cur.saturating_add(self.page.max(1));
        if next >= self.max {
            self.offset = None; // reaching the bottom resumes following
        } else {
            self.offset = Some(next);
        }
    }

    pub fn to_top(&mut self) {
        self.offset = Some(0);
    }

    pub fn to_bottom(&mut self) {
        self.offset = None;
    }
}

/// Whether keystrokes go to the chat input line or to a masked passphrase
/// prompt overlaid on it.
pub enum InputMode {
    Normal,
    Passphrase(PassphrasePrompt),
    Confirm(ConfirmPrompt),
}

/// A yes or no prompt asking whether to queue a message that could not be sent
/// right now. The message is held here until the user decides; on no it is
/// dropped without ever being sent or queued.
pub struct ConfirmPrompt {
    pub remote_static: [u8; 32],
    pub id: u64,
    pub text: String,
    pub label: String,
}

impl ConfirmPrompt {
    pub fn question(&self) -> String {
        let mut preview: String = self.text.chars().take(40).collect();
        if self.text.chars().count() > 40 {
            preview.push('…');
        }
        format!(
            "{} is offline. queue \"{}\" for delivery on reconnect? (y / n)",
            self.label, preview
        )
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum PassphrasePurpose {
    Create,
    Unlock,
}

pub enum PassphraseStage {
    Enter,
    Confirm { first: String },
    Waiting,
}

pub struct PassphrasePrompt {
    pub purpose: PassphrasePurpose,
    pub stage: PassphraseStage,
    pub buffer: String,
    pub error: Option<String>,
}

impl PassphrasePrompt {
    fn new(purpose: PassphrasePurpose) -> Self {
        Self {
            purpose,
            stage: PassphraseStage::Enter,
            buffer: String::new(),
            error: None,
        }
    }

    pub fn title(&self) -> &'static str {
        match self.stage {
            PassphraseStage::Waiting => "working…",
            PassphraseStage::Confirm { .. } => "confirm passphrase",
            PassphraseStage::Enter => match self.purpose {
                PassphrasePurpose::Create => "set a queue passphrase",
                PassphrasePurpose::Unlock => "unlock queue",
            },
        }
    }
}

impl App {
    pub fn new(identity: Identity) -> Self {
        let mut system_log: VecDeque<String> = VecDeque::with_capacity(SYSTEM_LOG_CAP);
        system_log.push_back("welcome to cord".to_string());
        if identity.freshly_generated {
            system_log.push_back(format!(
                "identity generated at {}",
                identity.config_dir.display()
            ));
            system_log.push_back(format!(
                "peer-id (full): {}. keep the config directory safe.",
                identity.peer_id
            ));
        } else {
            system_log.push_back(format!(
                "identity loaded from {}",
                identity.config_dir.display()
            ));
        }

        let contacts = match contacts::load(&identity.config_dir) {
            Ok(list) => {
                if !list.is_empty() {
                    system_log.push_back(format!("loaded {} contact(s) from disk", list.len()));
                }
                list
            }
            Err(e) => {
                system_log.push_back(format!("contacts: load failed: {e}"));
                Vec::new()
            }
        };

        system_log.push_back("ready. type /help for commands. Esc to quit.".to_string());
        Self {
            identity,
            transport_state: TransportState::Bootstrapping,
            peers: HashMap::new(),
            contacts,
            contacts_dirty: false,
            chat_log: VecDeque::with_capacity(CHAT_LOG_CAP),
            system_log,
            input_buffer: String::new(),
            mode: InputMode::Normal,
            vault_ready: false,
            vault_locked: false,
            connected: HashSet::new(),
            focus: Pane::Conversation,
            chat_view: PaneView::new(),
            log_view: PaneView::new(),
            should_quit: false,
        }
    }

    pub fn pair_with(&mut self, blob_text: &str) {
        let blob = match ContactBlob::decode(blob_text) {
            Ok(b) => b,
            Err(e) => {
                self.push_system(format!("/pair: {e}"));
                return;
            }
        };
        let label = blob.display_name.clone().unwrap_or_else(|| "(no name)".into());
        let existing = self
            .contacts
            .iter()
            .position(|c| c.blob.noise_static_pub == blob.noise_static_pub);
        match existing {
            Some(i) => match self.contacts[i].status {
                ContactStatus::Verified => {
                    self.push_system(format!(
                        "already verified: {}. use /unpair {} first if you want to re-pair.",
                        self.contacts[i].short_label(),
                        self.contacts[i].short_label()
                    ));
                }
                ContactStatus::Pending => {
                    self.push_system(format!(
                        "already pending: {}. compare SAS then /verify or /reject.",
                        self.contacts[i].short_label()
                    ));
                }
                ContactStatus::Rejected => {
                    self.contacts[i].blob = blob;
                    self.contacts[i].status = ContactStatus::Pending;
                    let saved = self.contacts[i].short_label();
                    if let Err(e) = contacts::save(&self.identity.config_dir, &self.contacts)
                    {
                        self.push_system(format!("contacts: save failed: {e}"));
                    }
                    self.push_system(format!(
                        "reopened previously rejected contact as pending: {saved}"
                    ));
                }
            },
            None => {
                self.contacts.push(Contact {
                    blob,
                    status: ContactStatus::Pending,
                });
                if let Err(e) = contacts::save(&self.identity.config_dir, &self.contacts) {
                    self.push_system(format!("contacts: save failed: {e}"));
                }
                self.push_system(format!("added pending contact: {label}"));
            }
        }
    }

    pub fn unpair_contact(&mut self, query: &str) {
        let matches: Vec<usize> = self
            .contacts
            .iter()
            .enumerate()
            .filter(|(_, c)| c.matches_query(query))
            .map(|(i, _)| i)
            .collect();
        match matches.as_slice() {
            [] => self.push_system(format!("no contact matches {query:?}")),
            [i] => {
                let removed = self.contacts.remove(*i);
                if let Err(e) = contacts::save(&self.identity.config_dir, &self.contacts) {
                    self.push_system(format!("contacts: save failed: {e}"));
                }
                self.contacts_dirty = true;
                self.push_system(format!("removed contact: {}", removed.short_label()));
            }
            _ => self.push_system(format!(
                "multiple contacts match {query:?}. be more specific."
            )),
        }
    }

    pub fn verify_contact(&mut self, query: &str) {
        self.transition_contact(query, ContactStatus::Verified, "verified", true);
    }

    pub fn reject_contact(&mut self, query: &str) {
        self.transition_contact(query, ContactStatus::Rejected, "rejected", false);
    }

    fn transition_contact(
        &mut self,
        query: &str,
        target: ContactStatus,
        verb: &str,
        require_pending: bool,
    ) {
        let matches: Vec<usize> = self
            .contacts
            .iter()
            .enumerate()
            .filter(|(_, c)| c.matches_query(query))
            .map(|(i, _)| i)
            .collect();
        match matches.as_slice() {
            [] => self.push_system(format!("no contact matches {query:?}")),
            [i] => {
                let i = *i;
                let current = self.contacts[i].status;
                if require_pending && current != ContactStatus::Pending {
                    let label = self.contacts[i].short_label();
                    self.push_system(format!(
                        "{label} is already {} (not pending)",
                        current.label()
                    ));
                    return;
                }
                self.contacts[i].status = target;
                let label = self.contacts[i].short_label();
                if let Err(e) = contacts::save(&self.identity.config_dir, &self.contacts) {
                    self.push_system(format!("contacts: save failed: {e}"));
                }
                self.contacts_dirty = true;
                self.push_system(format!("{verb} contact: {label}"));
            }
            _ => self.push_system(format!(
                "multiple contacts match {query:?}. use a more specific name or longer hex prefix."
            )),
        }
    }

    /// Verified contacts as routes for the runtime's retry loop.
    pub fn verified_routes(&self) -> Vec<ContactRoute> {
        self.contacts
            .iter()
            .filter(|c| c.status == ContactStatus::Verified)
            .map(|c| ContactRoute {
                remote_static: c.blob.noise_static_pub,
                hs_id: c.blob.hs_id,
                label: c.short_label(),
            })
            .collect()
    }

    pub fn list_contacts(&mut self) {
        if self.contacts.is_empty() {
            self.push_system("no contacts yet. use /pair <blob> to add one.");
            return;
        }
        self.push_system(format!("contacts ({}):", self.contacts.len()));
        let lines: Vec<String> = self
            .contacts
            .iter()
            .map(|c| format!("  {}", c))
            .collect();
        for line in lines {
            self.push_system(line);
        }
    }

    pub fn share_blob(&mut self, display_name: Option<String>) {
        let TransportState::Onion { hs_id, .. } = &self.transport_state else {
            self.push_system("/share: wait for Tor bootstrap to finish first.");
            return;
        };
        let noise_pub: [u8; 32] = *self.identity.noise_static.public_bytes();
        let blob = ContactBlob {
            noise_static_pub: noise_pub,
            hs_id: *hs_id,
            display_name,
        };
        self.push_system("share this with your peer:");
        self.push_system(blob.encode());
    }

    pub fn push_system(&mut self, line: impl Into<String>) {
        if self.system_log.len() == SYSTEM_LOG_CAP {
            self.system_log.pop_front();
        }
        self.system_log.push_back(line.into());
    }

    pub fn push_incoming(&mut self, from: String, text: String) {
        self.push_entry(ChatEntry::Incoming { from, text });
    }

    pub fn push_outgoing(&mut self, to: String, text: String, id: u64, status: DeliveryStatus) {
        self.push_entry(ChatEntry::Outgoing {
            to,
            text,
            id,
            status,
        });
    }

    fn push_entry(&mut self, entry: ChatEntry) {
        if self.chat_log.len() == CHAT_LOG_CAP {
            self.chat_log.pop_front();
        }
        self.chat_log.push_back(entry);
    }

    pub fn apply(&mut self, msg: AppMsg) {
        match msg {
            AppMsg::Log(text) => self.push_system(text),
            AppMsg::TransportReady(LocalAddrs { lan }) => {
                self.transport_state = match std::mem::replace(
                    &mut self.transport_state,
                    TransportState::Bootstrapping,
                ) {
                    TransportState::Onion {
                        onion_name, hs_id, ..
                    } => TransportState::Onion {
                        onion_name,
                        hs_id,
                        lan: Some(lan),
                    },
                    TransportState::BootstrappingTor {
                        percent, summary, ..
                    } => TransportState::BootstrappingTor {
                        percent,
                        summary,
                        lan: Some(lan),
                    },
                    _ => TransportState::Lan { listening_on: lan },
                };
                self.push_system(format!("lan listening on {lan}"));
            }
            AppMsg::TorProgress { percent, summary } => {
                let lan = match &self.transport_state {
                    TransportState::Lan { listening_on } => Some(*listening_on),
                    TransportState::BootstrappingTor { lan, .. } => *lan,
                    TransportState::Onion { lan, .. } => *lan,
                    _ => None,
                };
                self.transport_state = TransportState::BootstrappingTor {
                    percent,
                    summary,
                    lan,
                };
            }
            AppMsg::OnionReady { onion_name, hs_id } => {
                let lan = match &self.transport_state {
                    TransportState::Lan { listening_on } => Some(*listening_on),
                    TransportState::Onion { lan, .. } => *lan,
                    _ => None,
                };
                self.push_system(format!("onion ready: {onion_name}"));
                self.transport_state = TransportState::Onion {
                    onion_name,
                    hs_id,
                    lan,
                };
            }
            AppMsg::OnionFailed(error) => {
                self.transport_state = TransportState::Failed(format!("tor: {error}"));
                self.push_system(format!("tor: bootstrap failed: {error}"));
            }
            AppMsg::PeerDiscovered { peer_id, addr } => {
                self.peers.insert(peer_id, KnownPeer { addr });
                self.push_system(format!("discovered {} @ {addr}", peer_id.short()));
            }
            AppMsg::PeerLost(peer_id) => {
                if self.peers.remove(&peer_id).is_some() {
                    self.push_system(format!("lost {}", peer_id.short()));
                }
            }
            AppMsg::HandshakeOk { peer_id, role, sas, remote_static } => {
                self.connected.insert(remote_static);
                let matched: Option<(String, ContactStatus)> = self
                    .contacts
                    .iter()
                    .find(|c| c.blob.noise_static_pub == remote_static)
                    .map(|c| (c.short_label(), c.status));
                match matched {
                    // silent receive | a verified peer connecting is
                    // routine, an internal event only
                    Some((_, ContactStatus::Verified)) => {}
                    Some((label, ContactStatus::Pending)) => {
                        self.push_system(format!(
                            "handshake ok ({}): {}",
                            role.label(),
                            peer_id.short()
                        ));
                        self.push_system(format!("matches pending contact: {label}"));
                        self.push_system(format!(
                            "sas: {sas} (compare aloud, then /verify {label} or /reject {label})"
                        ));
                    }
                    Some((label, ContactStatus::Rejected)) => {
                        self.push_system(format!(
                            "handshake ok ({}): {}",
                            role.label(),
                            peer_id.short()
                        ));
                        self.push_system(format!(
                            "matches rejected contact: {label}. dropping is recommended."
                        ));
                        self.push_system(format!("sas: {sas}"));
                    }
                    None => {
                        self.push_system(format!(
                            "handshake ok ({}): {}",
                            role.label(),
                            peer_id.short()
                        ));
                        self.push_system(
                            "unpaired peer (no matching contact). /pair them first if you trust this connection.".to_string(),
                        );
                        self.push_system(format!("sas: {sas}"));
                    }
                }
            }
            AppMsg::HandshakeFailed { peer_id, role, error } => {
                let who = peer_id
                    .map(|p| p.short())
                    .unwrap_or_else(|| "<unknown>".to_string());
                self.push_system(format!(
                    "handshake failed ({} @ {who}): {error}",
                    role.label()
                ));
            }
            AppMsg::MessageReceived {
                remote_static,
                text,
                ..
            } => {
                let from = self.label_for_remote(&remote_static);
                self.push_incoming(from, text);
            }
            AppMsg::PeerDisconnected { remote_static, .. } => {
                self.connected.remove(&remote_static);
                // silent receive: a verified peer dropping must not surface either
                let verified = self.contacts.iter().any(|c| {
                    c.blob.noise_static_pub == remote_static
                        && matches!(c.status, ContactStatus::Verified)
                });
                if !verified {
                    let who = self.label_for_remote(&remote_static);
                    self.push_system(format!("disconnected: {who}"));
                }
            }
            AppMsg::DeliveryUpdate { id, status } => self.update_delivery(id, status),
            AppMsg::VaultLocked => {
                self.vault_locked = true;
                self.push_system(
                    "a saved message queue was found. enter your passphrase to unlock and resume pending deliveries (Esc to skip).",
                );
                if matches!(self.mode, InputMode::Normal) {
                    self.mode =
                        InputMode::Passphrase(PassphrasePrompt::new(PassphrasePurpose::Unlock));
                }
            }
            AppMsg::VaultReady => {
                self.vault_ready = true;
                self.vault_locked = false;
                self.mode = InputMode::Normal;
                self.push_system(
                    "offline queue ready. messages to an offline contact are held and delivered when they reconnect.",
                );
            }
            AppMsg::VaultFailed(msg) => {
                if let InputMode::Passphrase(p) = &mut self.mode {
                    p.stage = PassphraseStage::Enter;
                    p.buffer.clear();
                    p.error = Some(msg);
                } else {
                    self.push_system(format!("vault: {msg}"));
                }
            }
            AppMsg::QueueCleared { count } => {
                // Anything still showing as queued will not be delivered now.
                for entry in self.chat_log.iter_mut() {
                    if let ChatEntry::Outgoing { status, .. } = entry {
                        if *status == DeliveryStatus::Queued {
                            *status = DeliveryStatus::Dropped;
                        }
                    }
                }
                if count == 0 {
                    self.push_system("message queue is already empty");
                } else {
                    self.push_system(format!(
                        "cleared the message queue ({count} contact(s) had pending messages)"
                    ));
                }
            }
        }
    }

    fn label_for_remote(&self, remote_static: &[u8; 32]) -> String {
        self.contacts
            .iter()
            .find(|c| &c.blob.noise_static_pub == remote_static)
            .map(|c| c.short_label())
            .unwrap_or_else(|| "unknown".to_string())
    }

    fn update_delivery(&mut self, id: u64, status: DeliveryStatus) {
        // The matching message is almost always recent, so scan newest first.
        for entry in self.chat_log.iter_mut().rev() {
            if let ChatEntry::Outgoing {
                id: entry_id,
                status: entry_status,
                ..
            } = entry
            {
                if *entry_id == id {
                    *entry_status = status;
                    return;
                }
            }
        }
    }

    pub fn begin_passphrase_create(&mut self) {
        self.mode = InputMode::Passphrase(PassphrasePrompt::new(PassphrasePurpose::Create));
    }

    pub fn begin_passphrase_unlock(&mut self) {
        self.mode = InputMode::Passphrase(PassphrasePrompt::new(PassphrasePurpose::Unlock));
    }

    pub fn toggle_focus(&mut self) {
        self.focus = match self.focus {
            Pane::Conversation => Pane::SystemLog,
            Pane::SystemLog => Pane::Conversation,
        };
    }

    pub fn focused_view(&mut self) -> &mut PaneView {
        match self.focus {
            Pane::Conversation => &mut self.chat_view,
            Pane::SystemLog => &mut self.log_view,
        }
    }

    pub fn resolve_contact_onion(&self, query: &str) -> Result<String, String> {
        let matches: Vec<&Contact> = self
            .contacts
            .iter()
            .filter(|c| c.matches_query(query))
            .collect();
        let contact = match matches.as_slice() {
            [] => return Err(format!("no contact matches {query:?}")),
            [c] => *c,
            _ => return Err(format!(
                "multiple contacts match {query:?}. be more specific."
            )),
        };
        let hs_id = HsId::from(contact.blob.hs_id);
        let address = hs_id.display_unredacted().to_string();
        Ok(address)
    }

    pub fn send_to_contact(
        &mut self,
        query: &str,
        text: &str,
        cmd_tx: &mpsc::Sender<TransportCmd>,
    ) {
        let matches: Vec<usize> = self
            .contacts
            .iter()
            .enumerate()
            .filter(|(_, c)| c.matches_query(query))
            .map(|(i, _)| i)
            .collect();
        let i = match matches.as_slice() {
            [] => {
                self.push_system(format!("no contact matches {query:?}"));
                return;
            }
            [i] => *i,
            _ => {
                self.push_system(format!(
                    "multiple contacts match {query:?}. be more specific."
                ));
                return;
            }
        };
        if self.contacts[i].status != ContactStatus::Verified {
            let label = self.contacts[i].short_label();
            self.push_system(format!(
                "{label} is not verified. /verify them first."
            ));
            return;
        }
        let remote_static = self.contacts[i].blob.noise_static_pub;
        let label = self.contacts[i].short_label();
        let id = rand::random::<u64>();

        if self.connected.contains(&remote_static) {
            // a live connection exists: send straight away
            if cmd_tx
                .try_send(TransportCmd::SendMessage {
                    remote_static,
                    id,
                    text: text.to_string(),
                })
                .is_err()
            {
                self.push_system("send queue full");
                return;
            }
            self.push_outgoing(label, text.to_string(), id, DeliveryStatus::Sending);
        } else if self.vault_ready {
            // recipient offline but queueable: ask before queueing
            self.mode = InputMode::Confirm(ConfirmPrompt {
                remote_static,
                id,
                text: text.to_string(),
                label,
            });
        } else if self.vault_locked {
            self.push_system(format!(
                "{label} is offline and the queue is locked. /unlock first, then resend."
            ));
        } else {
            self.push_system(format!(
                "{label} is offline. set a passphrase with /passphrase to enable the offline queue, then resend."
            ));
        }
    }
}

pub async fn run(
    identity: Identity,
    mut msg_rx: mpsc::Receiver<AppMsg>,
    cmd_tx: mpsc::Sender<TransportCmd>,
) -> Result<(), CordError> {
    let mut terminal = setup_terminal()?;
    let result = run_loop(&mut terminal, identity, &mut msg_rx, &cmd_tx).await;
    let _ = cmd_tx.send(TransportCmd::Shutdown).await;
    restore_terminal(&mut terminal)?;
    result
}

async fn run_loop(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    identity: Identity,
    msg_rx: &mut mpsc::Receiver<AppMsg>,
    cmd_tx: &mpsc::Sender<TransportCmd>,
) -> Result<(), CordError> {
    let mut app = App::new(identity);
    let mut events = EventStream::new();
    let mut ticker = interval(Duration::from_millis(50));

    // initial route sync
    let _ = cmd_tx
        .send(TransportCmd::SyncContacts(app.verified_routes()))
        .await;

    loop {
        terminal.draw(|f| view::render(f, &mut app))?;
        if app.should_quit {
            return Ok(());
        }
        tokio::select! {
            Some(ev) = events.next() => {
                match ev {
                    Ok(event) => {
                        if let Some(cmd) = input::handle(&mut app, event, cmd_tx) {
                            let _ = cmd_tx.send(cmd).await;
                        }
                        // re-sync if the command changed the verified set
                        if app.contacts_dirty {
                            app.contacts_dirty = false;
                            let _ = cmd_tx
                                .send(TransportCmd::SyncContacts(app.verified_routes()))
                                .await;
                        }
                    }
                    Err(e) => return Err(e.into()),
                }
            }
            Some(msg) = msg_rx.recv() => app.apply(msg),
            _ = ticker.tick() => {}
        }
    }
}

fn setup_terminal() -> Result<Terminal<CrosstermBackend<Stdout>>, CordError> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    Ok(Terminal::new(backend)?)
}

fn restore_terminal(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
) -> Result<(), CordError> {
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    Ok(())
}
