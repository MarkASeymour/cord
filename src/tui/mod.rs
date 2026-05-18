use std::collections::{HashMap, VecDeque};
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
use crate::runtime::events::{AppMsg, LocalAddrs, TransportCmd};

pub mod input;
pub mod layout;
pub mod view;

const CHAT_LOG_CAP: usize = 500;

pub struct App {
    pub identity: Identity,
    pub transport_state: TransportState,
    pub peers: HashMap<PeerId, KnownPeer>,
    pub contacts: Vec<Contact>,
    pub chat_log: VecDeque<ChatEntry>,
    pub input_buffer: String,
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
    System(String),
    Incoming { from: String, text: String },
    Outgoing { to: String, text: String },
}

impl App {
    pub fn new(identity: Identity) -> Self {
        let mut chat_log = VecDeque::with_capacity(CHAT_LOG_CAP);
        chat_log.push_back(ChatEntry::System("welcome to cord".to_string()));
        if identity.freshly_generated {
            chat_log.push_back(ChatEntry::System(format!(
                "identity generated at {}",
                identity.config_dir.display()
            )));
            chat_log.push_back(ChatEntry::System(format!(
                "peer-id (full): {}. keep the config directory safe.",
                identity.peer_id
            )));
        } else {
            chat_log.push_back(ChatEntry::System(format!(
                "identity loaded from {}",
                identity.config_dir.display()
            )));
        }

        let contacts = match contacts::load(&identity.config_dir) {
            Ok(list) => {
                if !list.is_empty() {
                    chat_log.push_back(ChatEntry::System(format!(
                        "loaded {} contact(s) from disk",
                        list.len()
                    )));
                }
                list
            }
            Err(e) => {
                chat_log.push_back(ChatEntry::System(format!("contacts: load failed: {e}")));
                Vec::new()
            }
        };

        chat_log.push_back(ChatEntry::System(
            "ready. type /help for commands. Esc to quit.".to_string(),
        ));
        Self {
            identity,
            transport_state: TransportState::Bootstrapping,
            peers: HashMap::new(),
            contacts,
            chat_log,
            input_buffer: String::new(),
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
        if let Some(existing) = self
            .contacts
            .iter()
            .find(|c| c.blob.noise_static_pub == blob.noise_static_pub)
        {
            self.push_system(format!(
                "already paired with {} ({})",
                existing.short_label(),
                existing.status.label()
            ));
            return;
        }
        let label = blob.display_name.clone().unwrap_or_else(|| "(no name)".into());
        self.contacts.push(Contact {
            blob,
            status: ContactStatus::Pending,
        });
        if let Err(e) = contacts::save(&self.identity.config_dir, &self.contacts) {
            self.push_system(format!("contacts: save failed: {e}"));
        }
        self.push_system(format!("added pending contact: {label}"));
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
                self.push_system(format!("{verb} contact: {label}"));
            }
            _ => self.push_system(format!(
                "multiple contacts match {query:?}. use a more specific name or longer hex prefix."
            )),
        }
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
        let noise_pub: [u8; 32] = match self.identity.noise_static.as_bytes().try_into() {
            Ok(arr) => arr,
            Err(_) => {
                self.push_system("/share: noise key has wrong length (internal bug).");
                return;
            }
        };
        let blob = ContactBlob {
            noise_static_pub: noise_pub,
            hs_id: *hs_id,
            display_name,
        };
        self.push_system("share this with your peer:");
        self.push_system(blob.encode());
    }

    pub fn push_system(&mut self, line: impl Into<String>) {
        self.push_entry(ChatEntry::System(line.into()));
    }

    pub fn push_incoming(&mut self, from: String, text: String) {
        self.push_entry(ChatEntry::Incoming { from, text });
    }

    pub fn push_outgoing(&mut self, to: String, text: String) {
        self.push_entry(ChatEntry::Outgoing { to, text });
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
                self.push_system(format!(
                    "handshake ok ({}): {}",
                    role.label(),
                    peer_id.short()
                ));
                let matched: Option<(String, ContactStatus)> = self
                    .contacts
                    .iter()
                    .find(|c| c.blob.noise_static_pub == remote_static)
                    .map(|c| (c.short_label(), c.status));
                match matched {
                    Some((label, ContactStatus::Pending)) => {
                        self.push_system(format!("matches pending contact: {label}"));
                        self.push_system(format!(
                            "sas: {sas} (compare aloud, then /verify {label} or /reject {label})"
                        ));
                    }
                    Some((label, ContactStatus::Verified)) => {
                        self.push_system(format!("matches verified contact: {label}"));
                        self.push_system(format!("sas: {sas} (already verified)"));
                    }
                    Some((label, ContactStatus::Rejected)) => {
                        self.push_system(format!(
                            "matches rejected contact: {label}. dropping is recommended."
                        ));
                        self.push_system(format!("sas: {sas}"));
                    }
                    None => {
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
                let who = self.label_for_remote(&remote_static);
                self.push_system(format!("disconnected: {who}"));
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
        if cmd_tx
            .try_send(TransportCmd::SendMessage {
                remote_static,
                text: text.to_string(),
            })
            .is_err()
        {
            self.push_system("send queue full");
            return;
        }
        self.push_outgoing(label, text.to_string());
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

    loop {
        terminal.draw(|f| view::render(f, &app))?;
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
