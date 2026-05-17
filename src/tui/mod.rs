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
    pub chat_log: VecDeque<ChatEntry>,
    pub input_buffer: String,
    pub should_quit: bool,
}

pub enum TransportState {
    Bootstrapping,
    Lan {
        listening_on: SocketAddr,
    },
    Onion {
        onion_name: String,
        lan: Option<SocketAddr>,
    },
    Failed(String),
}

pub enum ChatEntry {
    System(String),
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
        chat_log.push_back(ChatEntry::System(
            "ready. type /help for commands. Esc to quit.".to_string(),
        ));
        Self {
            identity,
            transport_state: TransportState::Bootstrapping,
            peers: HashMap::new(),
            chat_log,
            input_buffer: String::new(),
            should_quit: false,
        }
    }

    pub fn push_system(&mut self, line: impl Into<String>) {
        if self.chat_log.len() == CHAT_LOG_CAP {
            self.chat_log.pop_front();
        }
        self.chat_log.push_back(ChatEntry::System(line.into()));
    }

    pub fn apply(&mut self, msg: AppMsg) {
        match msg {
            AppMsg::Log(text) => self.push_system(text),
            AppMsg::TransportReady(LocalAddrs { lan }) => {
                self.transport_state = match std::mem::replace(
                    &mut self.transport_state,
                    TransportState::Bootstrapping,
                ) {
                    TransportState::Onion { onion_name, .. } => TransportState::Onion {
                        onion_name,
                        lan: Some(lan),
                    },
                    _ => TransportState::Lan { listening_on: lan },
                };
                self.push_system(format!("lan listening on {lan}"));
            }
            AppMsg::OnionReady { onion_name } => {
                let lan = match &self.transport_state {
                    TransportState::Lan { listening_on } => Some(*listening_on),
                    TransportState::Onion { lan, .. } => *lan,
                    _ => None,
                };
                self.push_system(format!("onion ready: {onion_name}"));
                self.transport_state = TransportState::Onion {
                    onion_name,
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
            AppMsg::HandshakeOk { peer_id, role } => {
                self.push_system(format!(
                    "handshake ok ({}): {}",
                    role.label(),
                    peer_id.short()
                ));
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

    loop {
        terminal.draw(|f| view::render(f, &app))?;
        if app.should_quit {
            return Ok(());
        }
        tokio::select! {
            Some(ev) = events.next() => {
                match ev {
                    Ok(event) => {
                        if let Some(cmd) = input::handle(&mut app, event) {
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
