//! ChatGPT-authenticated Codex Responses provider.

use std::path::Path;
use std::sync::Arc;

use self::auth::BROWSER_AUTH;
use self::auth::ChatGptAuth;
use super::openai_auth::OpenAiAuthorization as _;
use super::openai_socket::DEFAULT_MODEL;
use super::openai_socket::MODELS;
use super::openai_socket::OpenAiSocket;
use super::openai_socket::SEARCH;
use super::provider::HostedWebSearch;
use super::provider::ProviderAuth;
use super::provider::ProviderBuildConfig;
use super::provider::ProviderDefinition;
use crate::Result;

#[path = "openai_codex_auth.rs"]
mod auth;

mod manifest {
    include!(concat!(
        env!("OUT_DIR"),
        "/src_backend_model_openai_codex_manifest.rs"
    ));
}

const PROVIDER_ID: &str = "openai_codex";
const HTTP_BASE_URL: &str = "https://chatgpt.com/backend-api/codex";
const SOCKET_URL: &str = "wss://chatgpt.com/backend-api/codex/responses";

/// ChatGPT identity used by the gateway's hosted Codex Apps runtime.
#[derive(Clone)]
pub struct AppsAuthorization(Arc<ChatGptAuth>);

impl AppsAuthorization {
    pub async fn resolve(&self) -> Result<(String, Vec<(String, String)>)> {
        let resolved = self.0.authorize_http(false, None).await?;
        Ok((resolved.token, resolved.headers))
    }

    pub async fn recover_unauthorized(&self, rejected_token: &str) -> Result<bool> {
        self.0.recover_unauthorized(rejected_token).await
    }
}

/// Loads the same owner-only ChatGPT credential used by the Codex provider.
pub fn apps_authorization(path: impl AsRef<Path>) -> Result<AppsAuthorization> {
    Ok(AppsAuthorization(Arc::new(ChatGptAuth::load(
        path.as_ref().to_path_buf(),
    )?)))
}

pub(super) const fn provider() -> ProviderDefinition {
    ProviderDefinition::new(
        PROVIDER_ID,
        manifest::PROVIDER_LABEL,
        "chat_gpt",
        manifest::PROVIDER_DESCRIPTION,
        ProviderAuth::Browser(&BROWSER_AUTH),
        MODELS,
        DEFAULT_MODEL,
        SEARCH,
        build_provider,
    )
    .with_image_input()
}

fn build_provider(config: ProviderBuildConfig) -> Result<Arc<dyn super::Model>> {
    let auth = config.credential.into_browser::<ChatGptAuth>(PROVIDER_ID)?;
    let provider = OpenAiSocket::with_authorization(
        auth,
        HTTP_BASE_URL,
        SOCKET_URL,
        config.model,
        config.http,
    )?;
    let provider = match config.reasoning_effort {
        Some(effort) => provider.with_reasoning_effort(effort)?,
        None => provider,
    };
    let provider = match config.web_search {
        HostedWebSearch::Off => provider,
        HostedWebSearch::Cached => provider.with_cached_web_search(),
        HostedWebSearch::Live => provider.with_web_search(),
    };
    Ok(Arc::new(provider))
}
