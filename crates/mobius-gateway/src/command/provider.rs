use mobius::backend::model::provider::provider;

use super::*;
use crate::config::ConfiguredProvider;
use crate::wire::{ProviderConfig, ProviderEndpointAuth, ProviderTint};

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
    let instance = options.instance.unwrap_or_else(|| options.provider.clone());
    let existing = config.configured_providers.get(&instance);
    let label = options
        .label
        .or_else(|| existing.map(|configured| configured.label.clone()))
        .unwrap_or_else(|| definition.label().to_owned());
    let tint = existing.map_or_else(ProviderTint::default, |configured| configured.tint);
    let reasoning_effort = options.reasoning_efforts.first().cloned();
    let selection = ProviderConfig {
        instance,
        provider: options.provider,
        model: options.model,
        base_url,
        endpoint_auth: if options.credentialless {
            ProviderEndpointAuth::Credentialless
        } else {
            ProviderEndpointAuth::ProviderDefault
        },
        reasoning_effort,
        web_search: options.web_search,
    };
    let model_ids = if definition.models().is_empty() {
        vec![selection.model.clone()]
    } else {
        Vec::new()
    };
    let replace_existing_selections = existing.is_some_and(|configured| {
        configured.selection != selection
            || configured.model_ids != model_ids
            || configured.reasoning_efforts != options.reasoning_efforts
    });
    let registration = ConfiguredProvider {
        selection: selection.clone(),
        label,
        tint,
        model_ids,
        reasoning_efforts: options.reasoning_efforts,
    };
    request_provider_registration(&endpoint, &token, registration, replace_existing_selections)
        .await?;
    println!("{}", register_provider_json(&selection.provider)?);
    Ok(())
}

pub(super) fn register_provider_json(provider: &str) -> Result<String> {
    Ok(serde_json::to_string(&RegisterProviderOutput { provider })?)
}

async fn request_provider_registration(
    endpoint: &Endpoint,
    token: &str,
    registration: ConfiguredProvider,
    replace_existing_selections: bool,
) -> Result<()> {
    let client = GatewayClient::connect(endpoint, token, ClientKind::GatewayDashboard).await?;
    let (sender, mut events) = client.into_parts();
    let request_id = Uuid::new_v4().to_string();
    sender
        .send(ClientMessage::RegisterProvider {
            request_id: request_id.clone(),
            config: registration.selection,
            label: registration.label,
            tint: registration.tint,
            model_ids: registration.model_ids,
            reasoning_efforts: registration.reasoning_efforts,
            replace_existing_selections,
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
