use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;

use arti_client::{TorClient, TorClientConfig};
use futures::Stream;
use safelog::DisplayRedacted;
use tor_hsservice::{config::OnionServiceConfigBuilder, HsNickname, RendRequest, RunningOnionService};
use tor_rtcompat::PreferredRuntime;

use super::TransportError;

pub type RendStream = Pin<Box<dyn Stream<Item = RendRequest> + Send>>;

pub struct OnionLaunch {
    pub onion_name: String,
    pub service: Arc<RunningOnionService>,
    pub tor_client: TorClient<PreferredRuntime>,
    pub rend_requests: RendStream,
}

pub async fn launch(_config_dir: PathBuf) -> Result<OnionLaunch, TransportError> {
    let config = TorClientConfig::default();
    let tor_client = TorClient::create_bootstrapped(config).await?;

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

    Ok(OnionLaunch {
        onion_name,
        service,
        tor_client,
        rend_requests: Box::pin(rend_requests),
    })
}
