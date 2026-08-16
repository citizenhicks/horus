use mobius::backend::model::provider::{HostedWebSearch, provider};

use super::*;
use crate::wire::{ProviderConfig, ProviderEndpointAuth};

pub(super) async fn register_provider_command(
    options: RegisterProviderOptions,
    load_local_client: fn(&Endpoint) -> Result<Option<String>>,
) -> Result<()> {
    let (_, config) = ConfigStore::open(options.state_dir)?;
    let endpoint = direct_loopback_endpoint(&config)?;
    let token = load_local_client(&endpoint)?
        .ok_or_else(|| Error::Config("gateway local control credential is unavailable".into()))?;
    let definition = provider(&options.provider)?;
    let base_url = options
        .base_url
        .or_else(|| definition.default_base_url().map(str::to_owned));
    let selection = ProviderConfig {
        provider: options.provider,
        model: options.model,
        base_url,
        endpoint_auth: if options.credentialless {
            ProviderEndpointAuth::Credentialless
        } else {
            ProviderEndpointAuth::ProviderDefault
        },
        reasoning_effort: None,
        web_search: HostedWebSearch::Off,
    };
    let model_ids = if definition.models().is_empty() {
        vec![selection.model.clone()]
    } else {
        Vec::new()
    };
    request_provider_registration(&endpoint, &token, selection.clone(), model_ids).await?;
    println!("{}", register_provider_json(&selection.provider)?);
    Ok(())
}

pub(super) fn register_provider_json(provider: &str) -> Result<String> {
    Ok(serde_json::to_string(&RegisterProviderOutput { provider })?)
}

async fn request_provider_registration(
    endpoint: &Endpoint,
    token: &str,
    config: ProviderConfig,
    model_ids: Vec<String>,
) -> Result<()> {
    let client = GatewayClient::connect(endpoint, token, ClientKind::GatewayDashboard).await?;
    let (sender, mut events) = client.into_parts();
    let request_id = Uuid::new_v4().to_string();
    sender
        .send(ClientMessage::RegisterProvider {
            request_id: request_id.clone(),
            config,
            model_ids,
            reasoning_efforts: Vec::new(),
            replace_existing_selections: true,
        })
        .await?;
    for _ in 0..MAX_PENDING_FRAMES {
        let frame = events.next().await?.ok_or_else(|| {
            Error::Protocol("gateway disconnected before registering the provider".into())
        })?;
        match frame.message {
            ServerMessage::GatewayConfigured {
                request_id: actual, ..
            } if actual == request_id => return Ok(()),
            ServerMessage::Rejected {
                request_id: actual,
                message,
                ..
            } if actual == request_id => return Err(Error::Protocol(message)),
            ServerMessage::Error { message, .. } => return Err(Error::Protocol(message)),
            _ => {}
        }
    }
    Err(Error::Protocol(format!(
        "gateway sent {MAX_PENDING_FRAMES} unrelated frames before the provider response"
    )))
}

#[derive(Serialize)]
struct RegisterProviderOutput<'a> {
    provider: &'a str,
}
