use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;

use arti_client::config::TorClientConfigBuilder;
use arti_client::TorClient;
use futures::{Stream, StreamExt};
use safelog::DisplayRedacted;
use tokio::sync::mpsc;
use tor_hsservice::{config::OnionServiceConfigBuilder, HsNickname, RendRequest, RunningOnionService};
use tor_rtcompat::PreferredRuntime;

use crate::runtime::events::AppMsg;

use super::TransportError;

pub type RendStream = Pin<Box<dyn Stream<Item = RendRequest> + Send>>;

pub struct OnionLaunch {
    pub onion_name: String,
    pub hs_id_bytes: [u8; 32],
    pub service: Arc<RunningOnionService>,
    pub tor_client: TorClient<PreferredRuntime>,
    pub rend_requests: RendStream,
}

pub async fn launch(
    config_dir: PathBuf,
    msg_tx: mpsc::Sender<AppMsg>,
) -> Result<OnionLaunch, TransportError> {
    // separate arti state per config dir, so two instances never share it
    let state_dir = config_dir.join("arti").join("state");
    let cache_dir = config_dir.join("arti").join("cache");
    let config = TorClientConfigBuilder::from_directories(state_dir, cache_dir)
        .build()
        .map_err(|e| TransportError::Onion(format!("tor config: {e}")))?;
    let tor_client = TorClient::builder()
        .config(config)
        .create_unbootstrapped()?;

    let mut events = tor_client.bootstrap_events();
    let progress_tx = msg_tx.clone();
    let _ = progress_tx
        .send(AppMsg::Log("tor: watching bootstrap events".into()))
        .await;
    let progress_task = tokio::spawn(async move {
        let mut last_reported = 255u8;
        let mut last_milestone = 0u8;
        let mut first = true;
        while let Some(status) = events.next().await {
            let percent = (status.as_frac() * 100.0).round() as u8;
            let summary = status.to_string();
            if first {
                first = false;
                let _ = progress_tx
                    .send(AppMsg::Log(format!(
                        "tor: first bootstrap event at {percent}% ({summary})"
                    )))
                    .await;
            }
            if percent != last_reported {
                last_reported = percent;
                if progress_tx
                    .send(AppMsg::TorProgress {
                        percent,
                        summary: summary.clone(),
                    })
                    .await
                    .is_err()
                {
                    break;
                }
            }
            if percent / 25 > last_milestone / 25 && percent > 0 {
                last_milestone = percent;
                let _ = progress_tx
                    .send(AppMsg::Log(format!("tor: bootstrap {percent}%")))
                    .await;
            }
            if status.ready_for_traffic() {
                let _ = progress_tx
                    .send(AppMsg::Log("tor: bootstrap complete".into()))
                    .await;
                break;
            }
        }
    });

    let bootstrap_result = tor_client.bootstrap().await;
    let _ = msg_tx
        .send(AppMsg::Log("tor: bootstrap call returned".into()))
        .await;
    progress_task.abort();
    bootstrap_result?;

    let nickname: HsNickname = "cord"
        .to_owned()
        .try_into()
        .map_err(|e: tor_hsservice::InvalidNickname| {
            TransportError::Onion(format!("invalid nickname: {e}"))
        })?;

    let onion_config = OnionServiceConfigBuilder::default()
        .nickname(nickname)
        .build()
        .map_err(|e| TransportError::Onion(format!("onion config: {e}")))?;

    let (service, rend_requests) = tor_client
        .launch_onion_service(onion_config)
        .map_err(|e| TransportError::Onion(format!("launch: {e}")))?
        .ok_or_else(|| TransportError::Onion("service disabled in config".into()))?;

    let hs_id = service
        .onion_address()
        .ok_or_else(|| TransportError::Onion("service has no onion address".into()))?;
    let onion_name = hs_id.display_unredacted().to_string();
    let hs_id_bytes: [u8; 32] = *hs_id.as_ref();

    Ok(OnionLaunch {
        onion_name,
        hs_id_bytes,
        service,
        tor_client,
        rend_requests: Box::pin(rend_requests),
    })
}
