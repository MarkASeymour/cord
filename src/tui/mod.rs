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
    pub conversations: HashMap<[u8; 32], Conversation>,
    pub active: Option<[u8; 32]>,
    pub system_log: VecDeque<String>,
    pub input_buffer: String,
    pub mode: InputMode,
    pub vault_ready: bool,
    pub vault_locked: bool,
    pub connected: HashSet<[u8; 32]>,
    pub show_log: bool,
    pub show_help: bool,
    pub log_unread: usize,
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

#[derive(Clone)]
pub enum ChatEntry {
    Incoming {
        from: String,
        text: String,
    },
    Outgoing {
        text: String,
        id: u64,
        status: DeliveryStatus,
    },
}

/// One contact's messages and unread tally, keyed in `App.conversations` by the
/// contact's Noise static key.
pub struct Conversation {
    pub entries: VecDeque<ChatEntry>,
    pub unread: usize,
}

impl Conversation {
    fn new() -> Self {
        Self {
            entries: VecDeque::new(),
            unread: 0,
        }
    }

    fn push(&mut self, entry: ChatEntry) {
        if self.entries.len() == CHAT_LOG_CAP {
            self.entries.pop_front();
        }
        self.entries.push_back(entry);
    }
}

/// The two scrollable panes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
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
    Sas(SasPrompt),
}

/// The pairing confirmation for a pending contact: shows the SAS to compare
/// aloud and captures keys until the user verifies or rejects.
pub struct SasPrompt {
    pub label: String,
    pub sas: String,
    pub remote_static: [u8; 32],
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

        system_log.push_back("ready. type /help for commands. Ctrl-C to quit.".to_string());
        let mut app = Self {
            identity,
            transport_state: TransportState::Bootstrapping,
            peers: HashMap::new(),
            contacts,
            contacts_dirty: false,
            conversations: HashMap::new(),
            active: None,
            system_log,
            input_buffer: String::new(),
            mode: InputMode::Normal,
            vault_ready: false,
            vault_locked: false,
            connected: HashSet::new(),
            show_log: false,
            show_help: false,
            log_unread: 0,
            focus: Pane::Conversation,
            chat_view: PaneView::new(),
            log_view: PaneView::new(),
            should_quit: false,
        };
        app.active = app.first_verified_key();
        app
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
        if !self.show_log {
            self.log_unread += 1;
        }
    }

    pub fn push_incoming(&mut self, remote_static: [u8; 32], from: String, text: String) {
        let unread = self.active != Some(remote_static);
        let convo = self
            .conversations
            .entry(remote_static)
            .or_insert_with(Conversation::new);
        convo.push(ChatEntry::Incoming { from, text });
        if unread {
            convo.unread += 1;
        }
    }

    pub fn push_outgoing(
        &mut self,
        remote_static: [u8; 32],
        text: String,
        id: u64,
        status: DeliveryStatus,
    ) {
        let convo = self
            .conversations
            .entry(remote_static)
            .or_insert_with(Conversation::new);
        convo.push(ChatEntry::Outgoing { text, id, status });
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
                        // open the pairing modal, unless another modal is up
                        if matches!(self.mode, InputMode::Normal) {
                            self.mode = InputMode::Sas(SasPrompt {
                                label,
                                sas,
                                remote_static,
                            });
                        } else {
                            self.push_system(format!(
                                "pending contact {label} connected. sas: {sas}. /verify or /reject when ready."
                            ));
                        }
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
                self.push_incoming(remote_static, from, text);
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
                for convo in self.conversations.values_mut() {
                    for entry in convo.entries.iter_mut() {
                        if let ChatEntry::Outgoing { status, .. } = entry {
                            if *status == DeliveryStatus::Queued {
                                *status = DeliveryStatus::Dropped;
                            }
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
        for convo in self.conversations.values_mut() {
            for entry in convo.entries.iter_mut().rev() {
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
    }

    pub fn begin_passphrase_create(&mut self) {
        self.mode = InputMode::Passphrase(PassphrasePrompt::new(PassphrasePurpose::Create));
    }

    pub fn begin_passphrase_unlock(&mut self) {
        self.mode = InputMode::Passphrase(PassphrasePrompt::new(PassphrasePurpose::Unlock));
    }

    pub fn toggle_log(&mut self) {
        self.show_log = !self.show_log;
        if !self.show_log && self.focus == Pane::SystemLog {
            self.focus = Pane::Conversation;
        }
    }

    pub fn toggle_focus(&mut self) {
        if !self.show_log {
            self.focus = Pane::Conversation;
            return;
        }
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

    /// First verified contact's Noise static key, in contact list order.
    fn first_verified_key(&self) -> Option<[u8; 32]> {
        self.contacts
            .iter()
            .find(|c| c.status == ContactStatus::Verified)
            .map(|c| c.blob.noise_static_pub)
    }

    fn set_active(&mut self, remote_static: [u8; 32]) {
        self.active = Some(remote_static);
        self.chat_view.to_bottom();
        if let Some(convo) = self.conversations.get_mut(&remote_static) {
            convo.unread = 0;
        }
    }

    /// Resolve a pairing from the SAS modal: verify or reject the contact by its
    /// Noise static key, persist, and resync routes. A verify also makes the
    /// contact active so the user can start talking.
    pub fn resolve_pairing(&mut self, remote_static: [u8; 32], verify: bool) {
        let Some(i) = self
            .contacts
            .iter()
            .position(|c| c.blob.noise_static_pub == remote_static)
        else {
            self.push_system("that contact no longer exists.");
            return;
        };
        self.contacts[i].status = if verify {
            ContactStatus::Verified
        } else {
            ContactStatus::Rejected
        };
        let label = self.contacts[i].short_label();
        if let Err(e) = contacts::save(&self.identity.config_dir, &self.contacts) {
            self.push_system(format!("contacts: save failed: {e}"));
        }
        self.contacts_dirty = true;
        if verify {
            self.set_active(remote_static);
            self.push_system(format!("verified contact: {label}. now talking to them."));
        } else {
            self.push_system(format!("rejected contact: {label}."));
        }
    }

    /// `/to <name>`: make a verified contact the active one so plain text goes
    /// to them.
    pub fn switch_to(&mut self, query: &str) {
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
            self.push_system(format!("{label} is not verified. /verify them first."));
            return;
        }
        let remote_static = self.contacts[i].blob.noise_static_pub;
        let label = self.contacts[i].short_label();
        self.set_active(remote_static);
        self.push_system(format!("now talking to {label}. just type to send."));
    }

    /// Send plain typed text to the active contact, defaulting to the first
    /// verified contact when none is active yet.
    pub fn send_active(&mut self, text: &str, cmd_tx: &mpsc::Sender<TransportCmd>) {
        if self.active.is_none() {
            self.active = self.first_verified_key();
        }
        let Some(remote_static) = self.active else {
            self.push_system("no verified contact yet. /pair someone, /verify them, then just type.");
            return;
        };
        let label = self
            .contacts
            .iter()
            .find(|c| {
                c.blob.noise_static_pub == remote_static && c.status == ContactStatus::Verified
            })
            .map(|c| c.short_label());
        let Some(label) = label else {
            self.active = None;
            self.push_system("the active contact is no longer verified. use /to <name>.");
            return;
        };
        self.dispatch_message(remote_static, label, text, cmd_tx);
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
        self.dispatch_message(remote_static, label, text, cmd_tx);
    }

    fn dispatch_message(
        &mut self,
        remote_static: [u8; 32],
        label: String,
        text: &str,
        cmd_tx: &mpsc::Sender<TransportCmd>,
    ) {
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
            self.push_outgoing(remote_static, text.to_string(), id, DeliveryStatus::Sending);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::noise::StaticKey;
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
    use std::sync::Arc;

    fn test_app() -> App {
        let identity = Identity {
            peer_id: PeerId::generate(),
            noise_static: Arc::new(StaticKey::generate().unwrap()),
            config_dir: std::env::temp_dir().join("cord-tui-step1-test"),
            freshly_generated: false,
        };
        App::new(identity)
    }

    fn contact(key: [u8; 32], name: &str, status: ContactStatus) -> Contact {
        Contact {
            blob: ContactBlob {
                noise_static_pub: key,
                hs_id: [0u8; 32],
                display_name: Some(name.to_string()),
            },
            status,
        }
    }

    #[test]
    fn messages_route_to_per_contact_conversations() {
        let mut app = test_app();
        let alice = [1u8; 32];
        let bob = [2u8; 32];

        app.push_incoming(alice, "alice".into(), "hi".into());
        app.push_outgoing(alice, "hey".into(), 10, DeliveryStatus::Sending);
        app.push_incoming(bob, "bob".into(), "yo".into());

        assert_eq!(app.conversations.len(), 2);
        assert_eq!(app.conversations[&alice].entries.len(), 2);
        assert_eq!(app.conversations[&bob].entries.len(), 1);
    }

    #[test]
    fn incoming_to_inactive_contact_counts_unread() {
        let mut app = test_app();
        let alice = [1u8; 32];

        // active is None, so an incoming message is unread
        app.push_incoming(alice, "alice".into(), "one".into());
        app.push_incoming(alice, "alice".into(), "two".into());
        assert_eq!(app.conversations[&alice].unread, 2);

        // the active contact's incoming does not add unread
        app.active = Some(alice);
        app.push_incoming(alice, "alice".into(), "three".into());
        assert_eq!(app.conversations[&alice].unread, 2);
    }

    #[test]
    fn conversation_preserves_insertion_order() {
        let mut app = test_app();
        let alice = [1u8; 32];

        app.push_incoming(alice, "alice".into(), "first".into());
        app.push_incoming(alice, "alice".into(), "second".into());

        let texts: Vec<String> = app.conversations[&alice]
            .entries
            .iter()
            .map(|e| match e {
                ChatEntry::Incoming { text, .. } | ChatEntry::Outgoing { text, .. } => text.clone(),
            })
            .collect();
        assert_eq!(texts, vec!["first", "second"]);
    }

    #[test]
    fn to_command_sets_active_and_clears_unread() {
        let mut app = test_app();
        let alice = [1u8; 32];
        app.contacts.push(contact(alice, "alice", ContactStatus::Verified));

        app.push_incoming(alice, "alice".into(), "hi".into());
        assert_eq!(app.conversations[&alice].unread, 1);

        app.switch_to("alice");
        assert_eq!(app.active, Some(alice));
        assert_eq!(app.conversations[&alice].unread, 0);
    }

    #[test]
    fn to_command_refuses_unverified_contact() {
        let mut app = test_app();
        let alice = [1u8; 32];
        app.contacts.push(contact(alice, "alice", ContactStatus::Pending));

        app.switch_to("alice");
        assert_eq!(app.active, None);
    }

    #[test]
    fn first_verified_key_skips_non_verified() {
        let mut app = test_app();
        let pending = [1u8; 32];
        let verified = [2u8; 32];
        app.contacts.push(contact(pending, "p", ContactStatus::Pending));
        app.contacts.push(contact(verified, "v", ContactStatus::Verified));

        assert_eq!(app.first_verified_key(), Some(verified));
    }

    #[test]
    fn typing_sends_to_active_contact_when_connected() {
        let mut app = test_app();
        let alice = [1u8; 32];
        app.contacts.push(contact(alice, "alice", ContactStatus::Verified));
        app.switch_to("alice");
        app.connected.insert(alice);

        let (tx, mut rx) = mpsc::channel(8);
        app.send_active("hello", &tx);

        match rx.try_recv() {
            Ok(TransportCmd::SendMessage { remote_static, text, .. }) => {
                assert_eq!(remote_static, alice);
                assert_eq!(text, "hello");
            }
            other => panic!("expected SendMessage, got {other:?}"),
        }
        assert_eq!(app.conversations[&alice].entries.len(), 1);
    }

    #[test]
    fn typing_with_no_verified_contact_does_nothing() {
        let mut app = test_app();
        let (tx, _rx) = mpsc::channel(8);

        app.send_active("hello", &tx);

        assert_eq!(app.active, None);
        assert!(app.conversations.is_empty());
    }

    #[test]
    fn delivery_update_finds_the_message_across_conversations() {
        let mut app = test_app();
        let alice = [1u8; 32];

        app.push_outgoing(alice, "hey".into(), 42, DeliveryStatus::Sending);
        app.update_delivery(42, DeliveryStatus::Delivered);

        match app.conversations[&alice].entries.back().unwrap() {
            ChatEntry::Outgoing { status, .. } => {
                assert_eq!(*status, DeliveryStatus::Delivered)
            }
            _ => panic!("expected outgoing entry"),
        }
    }

    fn type_and_enter(app: &mut App, text: &str, tx: &mpsc::Sender<TransportCmd>) {
        app.input_buffer = text.to_string();
        let key = Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()));
        input::handle(app, key, tx);
    }

    #[test]
    fn help_opens_a_panel_without_touching_the_log() {
        let mut app = test_app();
        let before = app.system_log.len();
        let (tx, _rx) = mpsc::channel(8);

        type_and_enter(&mut app, "/help", &tx);

        assert!(app.show_help);
        assert_eq!(app.system_log.len(), before, "help must not write to the log");
    }

    #[test]
    fn esc_closes_the_help_panel() {
        let mut app = test_app();
        app.show_help = true;
        let (tx, _rx) = mpsc::channel(8);

        let key = Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::empty()));
        input::handle(&mut app, key, &tx);

        assert!(!app.show_help);
    }

    #[test]
    fn another_command_dismisses_help() {
        let mut app = test_app();
        app.show_help = true;
        let (tx, _rx) = mpsc::channel(8);

        type_and_enter(&mut app, "hello", &tx);

        assert!(!app.show_help);
    }

    fn render_to_text(app: &mut App, w: u16, h: u16) -> String {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
        terminal.draw(|f| view::render(f, app)).unwrap();
        let buf = terminal.backend().buffer().clone();
        let mut s = String::new();
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                if let Some(cell) = buf.cell((x, y)) {
                    s.push_str(cell.symbol());
                }
            }
            s.push('\n');
        }
        s
    }

    #[test]
    fn wide_layout_shows_sidebar_and_active_messages() {
        let mut app = test_app();
        let alice = [1u8; 32];
        app.contacts.push(contact(alice, "alice", ContactStatus::Verified));
        app.switch_to("alice");
        app.push_incoming(alice, "alice".into(), "hello there".into());

        let text = render_to_text(&mut app, 80, 20);
        assert!(text.contains("contacts"), "sidebar header missing:\n{text}");
        assert!(text.contains("alice"), "contact name missing:\n{text}");
        assert!(text.contains("hello there"), "active message missing:\n{text}");
    }

    #[test]
    fn narrow_layout_hides_sidebar() {
        let mut app = test_app();
        let alice = [1u8; 32];
        app.contacts.push(contact(alice, "alice", ContactStatus::Verified));
        app.switch_to("alice");

        let text = render_to_text(&mut app, 50, 20);
        assert!(
            !text.contains("contacts"),
            "sidebar should be hidden on a narrow terminal:\n{text}"
        );
    }

    #[test]
    fn toggle_log_flips_and_moves_focus_off_hidden_log() {
        let mut app = test_app();
        app.show_log = true;
        app.focus = Pane::SystemLog;

        app.toggle_log();
        assert!(!app.show_log);
        assert_eq!(app.focus, Pane::Conversation);

        app.toggle_log();
        assert!(app.show_log);
    }

    #[test]
    fn hidden_log_drops_pane_and_shows_unread_count() {
        let mut app = test_app();
        app.push_system("boomtoken"); // log hidden by default, so this is unread

        let text = render_to_text(&mut app, 120, 20);
        assert!(!text.contains("system log"), "log pane should be hidden:\n{text}");
        assert!(
            text.contains("log 1"),
            "status bar should show the unread log count:\n{text}"
        );
    }

    #[test]
    fn system_messages_count_unread_until_log_is_shown() {
        let mut app = test_app();
        assert_eq!(app.log_unread, 0);

        app.push_system("a");
        app.push_system("b");
        assert_eq!(app.log_unread, 2);

        app.show_log = true;
        let _ = render_to_text(&mut app, 80, 20); // rendering the log clears it
        assert_eq!(app.log_unread, 0);
    }

    #[test]
    fn esc_clears_input_without_quitting() {
        let mut app = test_app();
        app.input_buffer.push_str("draft message");
        let (tx, _rx) = mpsc::channel(8);

        let key = Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::empty()));
        input::handle(&mut app, key, &tx);

        assert!(app.input_buffer.is_empty());
        assert!(!app.should_quit);
    }

    #[test]
    fn ctrl_l_toggles_the_log() {
        let mut app = test_app();
        let before = app.show_log;
        let (tx, _rx) = mpsc::channel(8);

        let key = Event::Key(KeyEvent::new(KeyCode::Char('l'), KeyModifiers::CONTROL));
        input::handle(&mut app, key, &tx);

        assert_eq!(app.show_log, !before);
    }

    #[test]
    fn pending_handshake_opens_sas_modal() {
        use crate::runtime::events::Role;
        let mut app = test_app();
        let alice = [1u8; 32];
        app.contacts.push(contact(alice, "alice", ContactStatus::Pending));

        app.apply(AppMsg::HandshakeOk {
            peer_id: PeerId::generate(),
            role: Role::Initiator,
            sas: "123 456 789".into(),
            remote_static: alice,
        });

        match &app.mode {
            InputMode::Sas(p) => {
                assert_eq!(p.remote_static, alice);
                assert_eq!(p.sas, "123 456 789");
            }
            _ => panic!("expected the SAS modal to open"),
        }
    }

    #[test]
    fn verified_handshake_opens_no_modal() {
        use crate::runtime::events::Role;
        let mut app = test_app();
        let alice = [1u8; 32];
        app.contacts.push(contact(alice, "alice", ContactStatus::Verified));

        app.apply(AppMsg::HandshakeOk {
            peer_id: PeerId::generate(),
            role: Role::Initiator,
            sas: "123 456 789".into(),
            remote_static: alice,
        });

        assert!(matches!(app.mode, InputMode::Normal));
    }

    #[test]
    fn sas_modal_verify_sets_verified_and_active() {
        let mut app = test_app();
        let alice = [1u8; 32];
        app.contacts.push(contact(alice, "alice", ContactStatus::Pending));
        app.mode = InputMode::Sas(SasPrompt {
            label: "alice".into(),
            sas: "123".into(),
            remote_static: alice,
        });
        let (tx, _rx) = mpsc::channel(8);

        let key = Event::Key(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::empty()));
        input::handle(&mut app, key, &tx);

        assert!(matches!(app.mode, InputMode::Normal));
        assert_eq!(app.contacts[0].status, ContactStatus::Verified);
        assert_eq!(app.active, Some(alice));
        assert!(app.contacts_dirty);
    }

    #[test]
    fn sas_modal_reject_sets_rejected_and_no_active() {
        let mut app = test_app();
        let alice = [1u8; 32];
        app.contacts.push(contact(alice, "alice", ContactStatus::Pending));
        app.mode = InputMode::Sas(SasPrompt {
            label: "alice".into(),
            sas: "123".into(),
            remote_static: alice,
        });
        let (tx, _rx) = mpsc::channel(8);

        let key = Event::Key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::empty()));
        input::handle(&mut app, key, &tx);

        assert_eq!(app.contacts[0].status, ContactStatus::Rejected);
        assert_eq!(app.active, None);
    }

    #[test]
    fn sas_modal_esc_defers_and_keeps_pending() {
        let mut app = test_app();
        let alice = [1u8; 32];
        app.contacts.push(contact(alice, "alice", ContactStatus::Pending));
        app.mode = InputMode::Sas(SasPrompt {
            label: "alice".into(),
            sas: "123".into(),
            remote_static: alice,
        });
        let (tx, _rx) = mpsc::channel(8);

        let key = Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::empty()));
        input::handle(&mut app, key, &tx);

        assert!(matches!(app.mode, InputMode::Normal));
        assert_eq!(app.contacts[0].status, ContactStatus::Pending);
    }
}
