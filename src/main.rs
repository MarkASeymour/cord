use std::path::PathBuf;
use std::process::ExitCode;

use argh::FromArgs;
use tokio::sync::mpsc;

use cord::errors::CordError;
use cord::identity;
use cord::runtime;
use cord::tui;

/// cord. serverless P2P terminal messenger over Tor onion services.
#[derive(FromArgs)]
struct Args {
    /// override the config directory. default uses the OS conventional location.
    #[argh(option)]
    config_dir: Option<PathBuf>,
}

#[tokio::main(flavor = "multi_thread")]
async fn main() -> ExitCode {
    rustls::crypto::ring::default_provider()
        .install_default()
        .expect("install rustls ring crypto provider");

    let args: Args = argh::from_env();
    match run(args).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("cord: {e}");
            ExitCode::FAILURE
        }
    }
}

async fn run(args: Args) -> Result<(), CordError> {
    let identity = identity::load_or_generate(args.config_dir)?;
    let (msg_tx, msg_rx) = mpsc::channel(64);
    let (cmd_tx, cmd_rx) = mpsc::channel(8);
    let transport = runtime::spawn(
        identity.peer_id,
        identity.config_dir.clone(),
        msg_tx,
        cmd_rx,
    )
    .await?;
    let result = tui::run(identity, msg_rx, cmd_tx).await;
    let _ = transport.handle.await;
    result
}
