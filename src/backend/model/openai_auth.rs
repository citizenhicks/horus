//! Authorization contract shared by OpenAI-compatible transports.

use std::sync::Arc;

use crate::BoxFuture;
use crate::Result;

pub(super) struct ResolvedAuthorization {
    pub token: String,
    pub headers: Vec<(String, String)>,
}

pub(super) trait OpenAiAuthorization: Send + Sync {
    fn authorize_http<'a>(
        &'a self,
        streaming: bool,
        session_id: Option<&'a str>,
    ) -> BoxFuture<'a, Result<ResolvedAuthorization>>;

    fn authorize_websocket<'a>(
        &'a self,
        session_id: &'a str,
    ) -> BoxFuture<'a, Result<ResolvedAuthorization>>;

    fn recover_unauthorized<'a>(&'a self, rejected_token: &'a str) -> BoxFuture<'a, Result<bool>>;
}

pub(super) struct ApiKeyAuthorization(Arc<str>);

impl ApiKeyAuthorization {
    pub fn new(api_key: String) -> Self {
        Self(api_key.into())
    }
}

impl OpenAiAuthorization for ApiKeyAuthorization {
    fn authorize_http<'a>(
        &'a self,
        _streaming: bool,
        _session_id: Option<&'a str>,
    ) -> BoxFuture<'a, Result<ResolvedAuthorization>> {
        self.resolved()
    }

    fn authorize_websocket<'a>(
        &'a self,
        _session_id: &'a str,
    ) -> BoxFuture<'a, Result<ResolvedAuthorization>> {
        self.resolved()
    }

    fn recover_unauthorized<'a>(&'a self, _rejected_token: &'a str) -> BoxFuture<'a, Result<bool>> {
        Box::pin(async { Ok(false) })
    }
}

impl ApiKeyAuthorization {
    fn resolved(&self) -> BoxFuture<'_, Result<ResolvedAuthorization>> {
        Box::pin(async move {
            Ok(ResolvedAuthorization {
                token: self.0.to_string(),
                headers: Vec::new(),
            })
        })
    }
}
