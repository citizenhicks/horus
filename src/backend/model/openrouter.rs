//! OpenRouter Responses provider.

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
use crate::protocol::FrontendSymbol;

const BASE_URL: &str = "https://openrouter.ai/api/v1";

const REASONING: &[ReasoningPreset] = &[
    ReasoningPreset {
        id: "low",
        label: "Low",
        description: "Prefer speed and lower cost",
    },
    ReasoningPreset {
        id: "medium",
        label: "Medium",
        description: "Balance reasoning and latency",
    },
    ReasoningPreset {
        id: "high",
        label: "High",
        description: "Prefer deeper reasoning",
    },
];

const MODELS: &[ModelPreset] = &[
    ModelPreset {
        id: "openrouter/pareto-code",
        label: "Pareto Code",
        description: "OpenRouter's coding-model router",
        context_window: 262_144,
        reasoning: &[],
        default_reasoning: None,
    },
    ModelPreset {
        id: "anthropic/claude-sonnet-5",
        label: "Claude Sonnet 5",
        description: "Anthropic model routed through OpenRouter",
        context_window: 1_000_000,
        reasoning: REASONING,
        default_reasoning: Some("high"),
    },
];

const SEARCH: &[HostedWebSearch] = &[HostedWebSearch::Off, HostedWebSearch::Live];

pub(super) const fn provider() -> ProviderDefinition {
    ProviderDefinition::new(
        "openrouter",
        "OpenRouter",
        FrontendSymbol::Route,
        "Responses API across multiple model vendors",
        ProviderAuth::ApiKey("OPENROUTER_API_KEY"),
        MODELS,
        SEARCH,
        build_provider,
    )
}

fn build_provider(config: ProviderBuildConfig) -> Result<Arc<dyn Model>> {
    let api_key = config.credential.into_api_key("openrouter")?;
    let provider = OpenAi::with_client(api_key, BASE_URL, config.model, config.http)?;
    let provider = match config.reasoning_effort {
        Some(effort) => provider.with_reasoning_effort(effort)?,
        None => provider,
    };
    let provider = if config.web_search == HostedWebSearch::Live {
        provider.with_hosted_tool(serde_json::json!({"type": "openrouter:web_search"}))
    } else {
        provider
    };
    Ok(Arc::new(provider))
}
