//! OpenRouter Responses provider.

use std::sync::Arc;

use super::Model;
use super::openai::OpenAi;
use super::provider::HostedWebSearch;
use super::provider::ProviderAuth;
use super::provider::ProviderBuildConfig;
use super::provider::ProviderDefinition;
use crate::Error;
use crate::Result;
use crate::protocol::FrontendSymbol;

mod manifest {
    include!(concat!(
        env!("OUT_DIR"),
        "/src_backend_model_openrouter_manifest.rs"
    ));
}

const BASE_URL: &str = "https://openrouter.ai/api/v1";

pub(super) const fn provider() -> ProviderDefinition {
    ProviderDefinition::new(
        "openrouter",
        manifest::PROVIDER_LABEL,
        FrontendSymbol::Route,
        manifest::PROVIDER_DESCRIPTION,
        ProviderAuth::ApiKey("OPENROUTER_API_KEY"),
        manifest::MODELS,
        manifest::DEFAULT_MODEL,
        manifest::SEARCH,
        build_provider,
    )
    .with_image_input()
    .with_base_url(BASE_URL)
    .with_credentialless_endpoints()
}

fn build_provider(config: ProviderBuildConfig) -> Result<Arc<dyn Model>> {
    let base_url = config
        .base_url
        .ok_or_else(|| Error::Config("OpenRouter requires a base URL".into()))?;
    let provider = match config.credential {
        super::provider::ProviderCredential::ApiKey(api_key) => {
            OpenAi::with_client(api_key, base_url, config.model, config.http)?
        }
        super::provider::ProviderCredential::Credentialless => {
            OpenAi::without_authorization(base_url, config.model, config.http)?
        }
        super::provider::ProviderCredential::Browser(_) => {
            return Err(Error::Config(
                "OpenRouter requires an API key or credentialless endpoint".into(),
            ));
        }
    };
    let provider = match config.reasoning_effort {
        Some(effort) => provider.with_reasoning_effort(effort)?,
        None => provider,
    };
    let provider = match config.web_search {
        HostedWebSearch::Off => provider,
        HostedWebSearch::Cached => {
            return Err(Error::Config(
                "OpenRouter does not support cached web search".into(),
            ));
        }
        HostedWebSearch::Live => {
            provider.with_hosted_tool(serde_json::json!({"type": "openrouter:web_search"}))
        }
    };
    Ok(Arc::new(provider))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::model::provider::ProviderCredential;

    #[test]
    fn advertised_web_search_modes_build() {
        let definition = provider();
        for web_search in definition.web_search().iter().copied() {
            definition
                .build(ProviderBuildConfig {
                    credential: ProviderCredential::ApiKey("test-key".into()),
                    model: "test-model".into(),
                    base_url: Some(BASE_URL.into()),
                    reasoning_effort: None,
                    web_search,
                    http: reqwest::Client::new(),
                })
                .expect("advertised web search mode builds");
        }
    }
}
