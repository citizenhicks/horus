use std::sync::{Arc, Mutex as StdMutex};

use horus::backend::model::provider::{ProviderAuth, provider};
use uuid::Uuid;

use crate::Error;
use crate::assembly::{configured_model_choices, credential_is_configured};
use crate::wire::{AgentComposition, ProviderConfig, ReadyPayload, ServerFrame, ServerMessage};

use super::{GatewayHost, Rejection, gateway_ready, internal, invalid_config};

impl GatewayHost {
    pub(crate) async fn configure_default_agent(
        &self,
        expected_revision: u64,
        config: AgentComposition,
    ) -> std::result::Result<ReadyPayload, Rejection> {
        let state = self.state.lock().await;
        {
            let mut current = state
                .config
                .lock()
                .map_err(|_| internal("gateway configuration lock is poisoned"))?;
            let models = configured_model_choices(&current, &state.store, &state.credentials)
                .map_err(internal)?;
            crate::middleware_manifest::validate_choices(&config.middleware, &models)
                .map_err(invalid_config)?;
            let next = current
                .replacing_default_agent(expected_revision, config)
                .map_err(invalid_config)?;
            state.store.save(&next).map_err(internal)?;
            *current = next;
        }
        let payload = gateway_ready(&state).await?;
        let _ = self.events.send(ServerFrame::new(ServerMessage::Ready {
            payload: payload.clone(),
        }));
        Ok(payload)
    }

    pub(crate) async fn set_credential(
        &self,
        provider_id: String,
        api_key: String,
        base_url: Option<String>,
    ) -> std::result::Result<(), Rejection> {
        let base_url = {
            let state = self.state.lock().await;
            let definition = provider(&provider_id).map_err(invalid_config)?;
            let base_url = if definition.configurable_base_url() {
                base_url.or_else(|| definition.default_base_url().map(str::to_owned))
            } else {
                base_url
            };
            definition
                .validate_base_url(base_url.as_deref())
                .map_err(invalid_config)?;
            state
                .credentials
                .set(&provider_id, &api_key, base_url.as_deref())
                .map_err(invalid_config)?;
            base_url
        };
        self.refresh_provider_sessions(&provider_id, base_url.as_deref())
            .await
    }

    pub(crate) async fn start_provider_login(
        &self,
        request_id: String,
        provider_id: String,
    ) -> std::result::Result<(), Rejection> {
        let definition = provider(&provider_id).map_err(invalid_config)?;
        let ProviderAuth::Browser(auth) = definition.auth() else {
            return Err(Rejection {
                code: "invalid_provider_auth",
                message: "the selected provider uses an API key".into(),
                fatal: false,
            });
        };
        if !auth.supports_device_login() {
            return Err(Rejection {
                code: "device_login_unavailable",
                message: "the selected provider does not support device-code login".into(),
                fatal: false,
            });
        }
        let (login_guard, path) = {
            let state = self.state.lock().await;
            (
                Arc::clone(&state.provider_login),
                state.store.provider_auth_path(),
            )
        };
        let login_id = Uuid::new_v4().to_string();
        reserve_provider_login(&login_guard, &login_id)?;
        let login = match auth.start_device().await {
            Ok(login) => login,
            Err(error) => {
                release_provider_login(&login_guard, &login_id)?;
                return Err(internal(error));
            }
        };
        self.broadcast(ServerMessage::ProviderLoginStarted {
            request_id: request_id.clone(),
            login_id: login_id.clone(),
            provider: provider_id.clone(),
            verification_url: login.verification_url().into(),
            user_code: login.user_code().into(),
        });
        let gateway = self.clone();
        tokio::spawn(async move {
            let result = login
                .complete(path)
                .await
                .map_err(|error| error.to_string());
            gateway
                .finish_provider_login(request_id, login_id, provider_id, result)
                .await;
        });
        Ok(())
    }

    async fn finish_provider_login(
        &self,
        request_id: String,
        login_id: String,
        provider: String,
        result: std::result::Result<(), String>,
    ) {
        let login_guard = Arc::clone(&self.state.lock().await.provider_login);
        match release_provider_login(&login_guard, &login_id) {
            Ok(true) => {}
            Ok(false) => return,
            Err(rejection) => {
                self.broadcast(ServerMessage::Error {
                    code: rejection.code.into(),
                    message: rejection.message,
                    fatal: rejection.fatal,
                });
                return;
            }
        }
        if let Err(message) = result {
            self.broadcast(ServerMessage::Rejected {
                request_id,
                code: "provider_login_failed".into(),
                message,
                fatal: false,
            });
            return;
        }
        let refresh = self.refresh_provider_sessions(&provider, None).await;
        self.broadcast(ServerMessage::ProviderLoginFinished {
            request_id,
            login_id,
            provider,
        });
        if let Err(rejection) = refresh {
            self.broadcast(ServerMessage::Error {
                code: rejection.code.into(),
                message: rejection.message,
                fatal: rejection.fatal,
            });
        }
    }

    async fn refresh_provider_sessions(
        &self,
        provider: &str,
        base_url: Option<&str>,
    ) -> std::result::Result<(), Rejection> {
        let sessions = self
            .state
            .lock()
            .await
            .sessions
            .values()
            .cloned()
            .collect::<Vec<_>>();
        let mut failure = None;
        for host in sessions {
            if let Err(rejection) = host
                .refresh_provider(provider.into(), base_url.map(str::to_owned))
                .await
            {
                failure.get_or_insert(rejection);
            }
        }
        failure.map_or(Ok(()), Err)
    }

    fn broadcast(&self, message: ServerMessage) {
        let _ = self.events.send(ServerFrame::new(message));
    }

    pub(crate) async fn register_provider(
        &self,
        selection: ProviderConfig,
        model_ids: Vec<String>,
        reasoning_efforts: Vec<String>,
    ) -> std::result::Result<ReadyPayload, Rejection> {
        let state = self.state.lock().await;
        if !credential_is_configured(&selection, &state.store, &state.credentials)
            .map_err(invalid_config)?
        {
            return Err(invalid_config(Error::Config(format!(
                "provider `{}` is not configured on this gateway",
                selection.provider
            ))));
        }
        {
            let mut current = state
                .config
                .lock()
                .map_err(|_| internal("gateway configuration lock is poisoned"))?;
            let next = current
                .registering_provider(selection, model_ids, reasoning_efforts)
                .map_err(invalid_config)?;
            state.store.save(&next).map_err(internal)?;
            *current = next;
        }
        let payload = gateway_ready(&state).await?;
        let frame = ServerFrame::new(ServerMessage::Ready {
            payload: payload.clone(),
        });
        let _ = self.events.send(frame);
        Ok(payload)
    }
}

fn ensure_provider_login_available(
    active_login: Option<&str>,
) -> std::result::Result<(), Rejection> {
    if active_login.is_some() {
        return Err(Rejection {
            code: "provider_login_in_progress",
            message: "finish the active provider login before starting another".into(),
            fatal: false,
        });
    }
    Ok(())
}

fn reserve_provider_login(
    active_login: &StdMutex<Option<String>>,
    login_id: &str,
) -> std::result::Result<(), Rejection> {
    let mut active_login = active_login
        .lock()
        .map_err(|_| internal("provider login lock is poisoned"))?;
    ensure_provider_login_available(active_login.as_deref())?;
    *active_login = Some(login_id.into());
    Ok(())
}

fn release_provider_login(
    active_login: &StdMutex<Option<String>>,
    login_id: &str,
) -> std::result::Result<bool, Rejection> {
    let mut active_login = active_login
        .lock()
        .map_err(|_| internal("provider login lock is poisoned"))?;
    if active_login.as_deref() != Some(login_id) {
        return Ok(false);
    }
    *active_login = None;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use crate::config::{ConfigStore, CredentialStore};
    use crate::cron::CronStore;

    use super::super::provider_credential_matches;
    use super::*;

    #[tokio::test]
    async fn credential_endpoints_are_validated_and_persisted() {
        let root = tempfile::tempdir().expect("root");
        let workspace = root.path().join("workspace");
        let state = root.path().join("state");
        std::fs::create_dir(&workspace).expect("workspace");
        let listen = "127.0.0.1:8741".parse().expect("listen address");
        let (store, config) = ConfigStore::initialize(state, listen, None).expect("config");
        let credentials =
            Arc::new(CredentialStore::open(store.credentials_path()).expect("credential store"));
        let cron = Arc::new(CronStore::open(store.state_dir()).expect("cron"));
        let gateway =
            GatewayHost::start(store, config, Arc::clone(&credentials), cron).expect("gateway");
        gateway.create_session(&workspace).await.expect("chat");
        let custom_endpoint = "https://example.com/v1";

        gateway
            .set_credential(
                "responses".into(),
                "custom-secret".into(),
                Some(custom_endpoint.into()),
            )
            .await
            .expect("store custom credential");
        let error = gateway
            .set_credential(
                "kimi".into(),
                "fixed-secret".into(),
                Some(custom_endpoint.into()),
            )
            .await
            .expect_err("fixed provider endpoint must be rejected");

        assert_eq!(
            credentials
                .get("responses", Some(custom_endpoint))
                .expect("custom credential"),
            Some("custom-secret".into())
        );
        assert_eq!(error.code, "invalid_config");
        assert!(error.message.contains("fixed API endpoint"));
        assert_eq!(
            credentials.get("kimi", None).expect("fixed credential"),
            None
        );
    }

    #[tokio::test]
    async fn credential_update_refreshes_every_matching_resident_chat() {
        let root = tempfile::tempdir().expect("root");
        let workspace = root.path().join("workspace");
        let state = root.path().join("state");
        std::fs::create_dir(&workspace).expect("workspace");
        let listen = "127.0.0.1:8741".parse().expect("listen address");
        let (store, config) = ConfigStore::initialize(state, listen, None).expect("config");
        let credentials =
            Arc::new(CredentialStore::open(store.credentials_path()).expect("credential store"));
        credentials
            .set("kimi", "old-secret", None)
            .expect("initial Kimi credential");
        let cron = Arc::new(CronStore::open(store.state_dir()).expect("cron"));
        let gateway = GatewayHost::start(store, config, credentials, cron).expect("gateway");
        gateway
            .register_provider(
                ProviderConfig {
                    provider: "kimi".into(),
                    model: "kimi-k3".into(),
                    base_url: None,
                    reasoning_effort: Some("max".into()),
                    web_search: horus::backend::model::provider::HostedWebSearch::Off,
                },
                Vec::new(),
                Vec::new(),
            )
            .await
            .expect("register Kimi");
        let first = gateway
            .create_session(&workspace)
            .await
            .expect("first chat");
        let second = gateway
            .create_session(&workspace)
            .await
            .expect("second chat");
        let mut first_events = first.subscribe();
        let mut second_events = second.subscribe();

        gateway
            .set_credential("kimi".into(), "new-secret".into(), None)
            .await
            .expect("replace Kimi credential");

        for events in [&mut first_events, &mut second_events] {
            tokio::time::timeout(Duration::from_secs(2), async {
                loop {
                    if matches!(
                        events.recv().await.expect("chat event").message,
                        ServerMessage::SessionChanged { .. }
                    ) {
                        break;
                    }
                }
            })
            .await
            .expect("matching chat refresh");
        }
    }

    #[test]
    fn credential_refresh_matches_only_the_selected_custom_endpoint() {
        let selection = ProviderConfig {
            provider: "responses".into(),
            model: "custom-model".into(),
            base_url: Some("https://first.example/v1".into()),
            reasoning_effort: None,
            web_search: horus::backend::model::provider::HostedWebSearch::Off,
        };

        assert!(
            provider_credential_matches(&selection, "responses", Some("https://first.example/v1"))
                .expect("matching endpoint")
                && !provider_credential_matches(
                    &selection,
                    "responses",
                    Some("https://second.example/v1")
                )
                .expect("different endpoint")
        );
    }

    #[test]
    fn active_provider_login_reserves_the_only_polling_slot() {
        let active = StdMutex::new(None);
        reserve_provider_login(&active, "login-a").expect("reserve first login");
        let rejection = reserve_provider_login(&active, "login-b")
            .expect_err("a second provider login must be rejected");

        assert_eq!(rejection.code, "provider_login_in_progress");
        release_provider_login(&active, "another-login").expect("ignore stale completion");
        assert!(reserve_provider_login(&active, "login-b").is_err());
        release_provider_login(&active, "login-a").expect("finish first login");
        reserve_provider_login(&active, "login-b").expect("reserve next login");
    }
}
