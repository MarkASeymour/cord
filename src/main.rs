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

fn main() -> ExitCode {
    rustls::crypto::ring::default_provider()
        .install_default()
        .expect("install rustls ring crypto provider");

    let args: Args = argh::from_env();

    // Capture the local UTC offset while still single threaded; `time` refuses to
    // read it once the multi threaded runtime is up. Timestamps are local only.
    let local_offset = time::UtcOffset::current_local_offset().unwrap_or(time::UtcOffset::UTC);

    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("cord: {e}");
            return ExitCode::FAILURE;
        }
    };

    match runtime.block_on(run(args, local_offset)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("cord: {e}");
            ExitCode::FAILURE
        }
    }
}

async fn run(args: Args, local_offset: time::UtcOffset) -> Result<(), CordError> {
    let identity = identity::load_or_generate(args.config_dir)?;
    let (msg_tx, msg_rx) = mpsc::channel(64);
    let (cmd_tx, cmd_rx) = mpsc::channel(8);
    let transport = runtime::spawn(&identity, msg_tx, cmd_rx).await?;
    let result = tui::run(identity, msg_rx, cmd_tx, local_offset).await;
    let _ = transport.handle.await;
    result
}
