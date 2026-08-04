//! DeepSeek Responses provider.

use std::sync::Arc;

use super::Model;
use super::openai::OpenAi;
use super::provider::HostedWebSearch;
use super::provider::ModelPreset;
use super::provider::ProviderAuth;
use super::provider::ProviderBuildConfig;
use super::provider::ProviderDefinition;
use super::provider::ReasoningPreset;
use crate::Result;

const BASE_URL: &str = "https://api.deepseek.com";

const REASONING: &[ReasoningPreset] = &[
    ReasoningPreset {
        id: "low",
        label: "Low",
        description: "Prefer speed and lower cost",
    },
    ReasoningPreset {
        id: "high",
        label: "High",
        description: "DeepSeek's default reasoning effort",
    },
    ReasoningPreset {
        id: "max",
        label: "Maximum",
        description: "Use maximum available reasoning",
    },
];

const MODELS: &[ModelPreset] = &[ModelPreset {
    id: "deepseek-v4-flash",
    label: "DeepSeek V4 Flash",
    description: "DeepSeek's fast frontier agentic model",
    context_window: 1_000_000,
    reasoning: REASONING,
    default_reasoning: Some("high"),
}];

const SEARCH: &[HostedWebSearch] = &[HostedWebSearch::Off, HostedWebSearch::Live];

pub(super) const fn provider() -> ProviderDefinition {
    ProviderDefinition::new(
        "deepseek",
        "DeepSeek",
        "magnifying-glass",
        "DeepSeek Responses API",
        ProviderAuth::ApiKey("DEEPSEEK_API_KEY"),
        MODELS,
        SEARCH,
        build_provider,
    )
}

fn build_provider(config: ProviderBuildConfig) -> Result<Arc<dyn Model>> {
    let api_key = config.credential.into_api_key("deepseek")?;
    let provider = OpenAi::with_client(api_key, BASE_URL, config.model, config.http)?;
    let provider = match config.reasoning_effort {
        Some(effort) => provider.with_reasoning_effort(effort)?,
        None => provider,
    };
    let provider = if config.web_search == HostedWebSearch::Live {
        provider.with_web_search()
    } else {
        provider
    };
    Ok(Arc::new(provider))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::model::provider::ProviderCredential;
    use crate::backend::model::provider::provider as registered_provider;

    #[test]
    fn registered_provider_builds_its_default_model() {
        let definition = registered_provider("deepseek").expect("registered provider");
        let model = definition
            .build(ProviderBuildConfig {
                credential: ProviderCredential::ApiKey("test-key".into()),
                model: "deepseek-v4-flash".into(),
                base_url: None,
                reasoning_effort: None,
                web_search: HostedWebSearch::Off,
                http: reqwest::Client::new(),
            })
            .expect("build provider");

        assert!(matches!(
            definition.auth(),
            ProviderAuth::ApiKey("DEEPSEEK_API_KEY")
        ));
        assert_eq!(definition.models(), MODELS);
        assert_eq!(definition.web_search(), SEARCH);
        assert_eq!(model.info().reasoning_effort.as_deref(), Some("high"));
    }
}
