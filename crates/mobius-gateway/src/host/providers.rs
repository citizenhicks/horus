use std::collections::HashSet;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex as StdMutex};

use mobius::backend::checkpoint::CheckpointStore;
use mobius::backend::model::provider::{ProviderAuth, provider};
use uuid::Uuid;

use crate::Error;
use crate::assembly::{configured_model_choices, credential_is_configured};
use crate::config::{ChatSpec, GatewayConfig};
use crate::wire::{
    AgentComposition, ProviderConfig, ProviderTint, ReadyPayload, ServerFrame, ServerMessage,
};

use super::session::ProviderRefresh;
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
        instance: String,
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
                .set(&instance, &provider_id, &api_key, base_url.as_deref())
                .map_err(invalid_config)?;
            base_url
        };
        self.refresh_provider_sessions(ProviderRefresh::Instance { instance, base_url })
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
        let refresh = self
            .refresh_provider_sessions(ProviderRefresh::Provider(provider.clone()))
            .await;
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
        scope: ProviderRefresh,
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
            if let Err(rejection) = host.refresh_provider(scope.clone()).await {
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
        label: String,
        tint: ProviderTint,
        model_ids: Vec<String>,
        reasoning_efforts: Vec<String>,
        replace_existing_selections: bool,
    ) -> std::result::Result<ReadyPayload, Rejection> {
        let mut state = self.state.lock().await;
        let mutation_gate = Arc::clone(&state.session_mutations);
        let _mutation = mutation_gate.write_owned().await;
        if !credential_is_configured(&selection, &state.store, &state.credentials)
            .map_err(invalid_config)?
        {
            return Err(invalid_config(Error::Config(format!(
                "provider `{}` is not configured on this gateway",
                selection.provider
            ))));
        }
        let current = state
            .config
            .lock()
            .map_err(|_| internal("gateway configuration lock is poisoned"))?
            .clone();
        let next = provider_registration(
            &current,
            &selection,
            &label,
            tint,
            &model_ids,
            &reasoning_efforts,
            replace_existing_selections,
        )
        .map_err(invalid_config)?;
        let mut replacements = Vec::new();
        let mut migrations = Vec::new();
        let mut target_epoch = state.provider_epoch.load(Ordering::Acquire);
        if current.configured_providers != next.configured_providers {
            target_epoch = target_epoch
                .checked_add(1)
                .ok_or_else(|| internal("provider catalog epoch overflow"))?;
        }
        if replace_existing_selections {
            let residents = provider_cutover_residents(&mut state).await?;
            let resident_ids = residents
                .iter()
                .map(|resident| resident.session_id.clone())
                .collect::<HashSet<_>>();
            migrations = residents
                .iter()
                .filter(|resident| {
                    resident.status.provider_epoch != target_epoch
                        || (resident.status.selection.instance == selection.instance
                            && resident.status.selection != selection)
                })
                .map(|resident| (resident.session_id.clone(), resident.host.clone()))
                .collect();
            replacements =
                provider_checkpoint_replacements(&state, &selection, &next, &resident_ids)
                    .await
                    .map_err(internal)?;
            if current == next && replacements.is_empty() && migrations.is_empty() {
                return gateway_ready(&state).await;
            }
            if residents.iter().any(|resident| !resident.status.idle) {
                return Err(Rejection {
                    code: "agent_busy",
                    message: "finish or interrupt active turns before changing gateway providers"
                        .into(),
                    fatal: false,
                });
            }
            save_provider_checkpoint_replacements(&state.checkpoints, &replacements)
                .await
                .map_err(internal)?;
        }
        let commit = commit_provider_registration(
            &state,
            &selection,
            &label,
            tint,
            &model_ids,
            &reasoning_efforts,
            replace_existing_selections,
        );
        let catalog_changed = match commit {
            Ok(catalog_changed) => catalog_changed,
            Err(error) => {
                if replace_existing_selections
                    && let Err(rollback) =
                        rollback_provider_checkpoints(&state.checkpoints, &replacements).await
                {
                    return Err(internal(format!(
                        "{error}; failed to roll back provider chat selections: {rollback}"
                    )));
                }
                return Err(internal(error));
            }
        };
        if catalog_changed {
            state.provider_epoch.store(target_epoch, Ordering::Release);
        }
        for (session_id, host) in migrations {
            host.cut_over_provider(&selection).await?;
            if !host.is_alive() {
                state.sessions.remove(&session_id);
                return Err(internal("chat stopped during provider replacement"));
            }
        }
        let payload = gateway_ready(&state).await?;
        let frame = ServerFrame::new(ServerMessage::Ready {
            payload: payload.clone(),
        });
        let _ = self.events.send(frame);
        Ok(payload)
    }

    pub(crate) async fn remove_provider(
        &self,
        instance: String,
    ) -> std::result::Result<ReadyPayload, Rejection> {
        let mut state = self.state.lock().await;
        let mutation_gate = Arc::clone(&state.session_mutations);
        let _mutation = mutation_gate.write_owned().await;
        state
            .config
            .lock()
            .map_err(|_| internal("gateway configuration lock is poisoned"))?
            .removing_provider(&instance)
            .map_err(invalid_config)?;
        let target_epoch = state
            .provider_epoch
            .load(Ordering::Acquire)
            .checked_add(1)
            .ok_or_else(|| internal("provider catalog epoch overflow"))?;
        let residents = provider_cutover_residents(&mut state).await?;
        if residents.iter().any(|resident| !resident.status.idle) {
            return Err(Rejection {
                code: "agent_busy",
                message: "finish or interrupt active turns before removing a gateway provider"
                    .into(),
                fatal: false,
            });
        }
        commit_provider_removal(&state, &instance).map_err(internal)?;
        state.provider_epoch.store(target_epoch, Ordering::Release);
        let mut reload_failures = Vec::new();
        for resident in residents {
            if let Err(error) = resident.host.reload_provider_catalog().await {
                if !resident.host.stop_if_idle().await {
                    return Err(internal(
                        "chat became busy after provider removal was committed",
                    ));
                }
                state.sessions.remove(&resident.session_id);
                reload_failures.push(format!("{}: {}", resident.session_id, error.message));
            }
        }
        let payload = gateway_ready(&state).await?;
        let _ = self.events.send(ServerFrame::new(ServerMessage::Ready {
            payload: payload.clone(),
        }));
        if !reload_failures.is_empty() {
            let _ = self.events.send(ServerFrame::new(ServerMessage::Error {
                code: "provider_reload".into(),
                message: format!(
                    "provider removed; reopen chats that could not reload: {}",
                    reload_failures.join(", ")
                ),
                fatal: false,
            }));
        }
        Ok(payload)
    }
}

fn provider_registration(
    current: &GatewayConfig,
    selection: &ProviderConfig,
    label: &str,
    tint: ProviderTint,
    model_ids: &[String],
    reasoning_efforts: &[String],
    replace_existing_selections: bool,
) -> crate::Result<GatewayConfig> {
    let registered = current.registering_provider(
        selection.clone(),
        label.to_owned(),
        tint,
        model_ids.to_vec(),
        reasoning_efforts.to_vec(),
    )?;
    if replace_existing_selections {
        registered.replacing_provider_default(selection)
    } else {
        Ok(registered)
    }
}

fn commit_provider_registration(
    state: &super::GatewayState,
    selection: &ProviderConfig,
    label: &str,
    tint: ProviderTint,
    model_ids: &[String],
    reasoning_efforts: &[String],
    replace_existing_selections: bool,
) -> crate::Result<bool> {
    let mut current = state
        .config
        .lock()
        .map_err(|_| Error::Config("gateway configuration lock is poisoned".into()))?;
    let next = provider_registration(
        &current,
        selection,
        label,
        tint,
        model_ids,
        reasoning_efforts,
        replace_existing_selections,
    )?;
    let catalog_changed = current.configured_providers != next.configured_providers;
    state.store.save(&next)?;
    *current = next;
    Ok(catalog_changed)
}

fn commit_provider_removal(state: &super::GatewayState, instance: &str) -> crate::Result<()> {
    let mut current = state
        .config
        .lock()
        .map_err(|_| Error::Config("gateway configuration lock is poisoned".into()))?;
    let next = current.removing_provider(instance)?;
    state.store.save(&next)?;
    if let Err(error) = state.credentials.remove(instance) {
        if let Err(rollback) = state.store.save(&current) {
            return Err(Error::Config(format!(
                "{error}; failed to roll back provider configuration: {rollback}"
            )));
        }
        return Err(error);
    }
    *current = next;
    Ok(())
}

struct ProviderCheckpointReplacement {
    session_id: String,
    sequence: u64,
    original: ChatSpec,
    updated: ChatSpec,
}

struct ProviderCutoverResident {
    session_id: String,
    host: super::HostHandle,
    status: super::ProviderCutoverStatus,
}

async fn provider_cutover_residents(
    state: &mut super::GatewayState,
) -> std::result::Result<Vec<ProviderCutoverResident>, Rejection> {
    let sessions = state
        .sessions
        .iter()
        .map(|(id, host)| (id.clone(), host.clone()))
        .collect::<Vec<_>>();
    let mut residents = Vec::new();
    let mut stopped = Vec::new();
    for (id, host) in sessions {
        if !host.is_alive() {
            stopped.push(id);
            continue;
        }
        match host.provider_cutover_status().await {
            Ok(status) => residents.push(ProviderCutoverResident {
                session_id: id,
                host,
                status,
            }),
            Err(rejection) if rejection.code == "gateway_stopped" => stopped.push(id),
            Err(rejection) => return Err(rejection),
        }
    }
    for id in stopped {
        state.sessions.remove(&id);
    }
    Ok(residents)
}

async fn provider_checkpoint_replacements(
    state: &super::GatewayState,
    selection: &ProviderConfig,
    gateway: &GatewayConfig,
    excluded: &HashSet<String>,
) -> crate::Result<Vec<ProviderCheckpointReplacement>> {
    let sessions = super::gateway_session_summaries(&state.checkpoints).await?;
    let mut replacements = Vec::new();
    for session in sessions {
        if excluded.contains(&session.session_id) {
            continue;
        }
        let Some(checkpoint) = state.checkpoints.load(&session.session_id).await? else {
            continue;
        };
        let Some(original) = ChatSpec::from_metadata_if_present(
            &checkpoint.metadata,
            state.store.state_dir(),
            gateway.tls.as_ref(),
        )?
        else {
            continue;
        };
        let Some(updated) = original.replacing_provider_selection(
            selection,
            gateway,
            state.store.state_dir(),
            gateway.tls.as_ref(),
        )?
        else {
            continue;
        };
        replacements.push(ProviderCheckpointReplacement {
            session_id: session.session_id,
            sequence: checkpoint.sequence,
            original,
            updated,
        });
    }
    Ok(replacements)
}

async fn save_provider_checkpoint_replacements(
    checkpoints: &Arc<dyn CheckpointStore>,
    replacements: &[ProviderCheckpointReplacement],
) -> crate::Result<()> {
    for (index, replacement) in replacements.iter().enumerate() {
        let result = async {
            let mut checkpoint = checkpoints
                .load(&replacement.session_id)
                .await?
                .ok_or_else(|| {
                    Error::Config("chat disappeared during provider replacement".into())
                })?;
            if checkpoint.sequence != replacement.sequence {
                return Err(Error::Config(
                    "chat changed during provider replacement".into(),
                ));
            }
            checkpoint.sequence = checkpoint
                .sequence
                .checked_add(1)
                .ok_or_else(|| Error::Config("checkpoint sequence overflow".into()))?;
            checkpoint.metadata.extend(replacement.updated.metadata()?);
            checkpoints.save(&checkpoint, &[], None).await?;
            Ok(())
        }
        .await;
        if let Err(error) = result {
            if let Err(rollback) =
                rollback_provider_checkpoints(checkpoints, &replacements[..index]).await
            {
                return Err(Error::Config(format!(
                    "{error}; failed to roll back provider chat selections: {rollback}"
                )));
            }
            return Err(error);
        }
    }
    Ok(())
}

async fn rollback_provider_checkpoints(
    checkpoints: &Arc<dyn CheckpointStore>,
    replacements: &[ProviderCheckpointReplacement],
) -> crate::Result<()> {
    for replacement in replacements.iter().rev() {
        let mut checkpoint = checkpoints
            .load(&replacement.session_id)
            .await?
            .ok_or_else(|| Error::Config("chat disappeared during provider rollback".into()))?;
        let replaced_sequence = replacement
            .sequence
            .checked_add(1)
            .ok_or_else(|| Error::Config("checkpoint sequence overflow".into()))?;
        if checkpoint.sequence != replaced_sequence {
            return Err(Error::Config(
                "chat changed during provider rollback".into(),
            ));
        }
        checkpoint.sequence = checkpoint
            .sequence
            .checked_add(1)
            .ok_or_else(|| Error::Config("checkpoint sequence overflow".into()))?;
        checkpoint.metadata.extend(replacement.original.metadata()?);
        checkpoints.save(&checkpoint, &[], None).await?;
    }
    Ok(())
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
    use std::sync::atomic::AtomicBool;
    use std::time::Duration;

    use mobius::protocol::FrontendSettingValue;
    use tokio::sync::{broadcast, mpsc};

    use crate::config::{ConfigStore, CredentialStore};
    use crate::cron::CronStore;

    use super::super::HostHandle;
    use super::super::session::{
        HostCommand, HostInner, ProviderCutoverStatus, ProviderRefresh, provider_refresh_matches,
    };
    use super::*;

    fn provider_removal_gateway(
        root: &tempfile::TempDir,
    ) -> (
        GatewayHost,
        Arc<CredentialStore>,
        ProviderConfig,
        ProviderConfig,
    ) {
        let state_dir = root.path().join("state");
        let listen = "127.0.0.1:8741".parse().expect("listen address");
        let (store, config) = ConfigStore::initialize(state_dir, listen, None).expect("config");
        let primary = ProviderConfig {
            instance: "openrouter".into(),
            provider: "openrouter".into(),
            model: "openai/gpt-5".into(),
            base_url: Some("https://connector.example/v1".into()),
            endpoint_auth: crate::wire::ProviderEndpointAuth::Credentialless,
            reasoning_effort: None,
            web_search: mobius::backend::model::provider::HostedWebSearch::Off,
        };
        let removable = ProviderConfig {
            instance: "kimi-unused".into(),
            provider: "kimi".into(),
            model: "kimi-k3".into(),
            base_url: Some("https://api.moonshot.ai/v1".into()),
            endpoint_auth: crate::wire::ProviderEndpointAuth::ProviderDefault,
            reasoning_effort: Some("max".into()),
            web_search: mobius::backend::model::provider::HostedWebSearch::Off,
        };
        let route = crate::config::model_route_id(
            &removable.instance,
            &removable.model,
            removable.reasoning_effort.as_deref(),
        );
        let config = config
            .registering_provider(
                primary.clone(),
                "Primary".into(),
                Default::default(),
                vec![primary.model.clone()],
                Vec::new(),
            )
            .and_then(|config| {
                config.registering_provider(
                    removable.clone(),
                    "Unused".into(),
                    Default::default(),
                    Vec::new(),
                    Vec::new(),
                )
            })
            .expect("provider catalog");
        let mut default = config
            .default_agent
            .as_ref()
            .expect("default")
            .config
            .clone();
        default.middleware.set_setting(
            "subagents",
            "model_route",
            Some(FrontendSettingValue::String(route.clone())),
        );
        let config = config
            .replacing_default_agent(1, default)
            .expect("middleware route default");
        store.save(&config).expect("save provider catalog");
        let credentials =
            Arc::new(CredentialStore::open(store.credentials_path()).expect("credentials"));
        credentials
            .set(
                &removable.instance,
                &removable.provider,
                "unused-secret",
                removable.base_url.as_deref(),
            )
            .expect("removable credential");
        let cron = Arc::new(CronStore::open(store.state_dir()).expect("cron"));
        let gateway =
            GatewayHost::start(store, config, Arc::clone(&credentials), cron).expect("gateway");
        (gateway, credentials, primary, removable)
    }

    #[tokio::test]
    async fn provider_removal_rejects_the_primary_default_without_changes() {
        let root = tempfile::tempdir().expect("root");
        let (gateway, credentials, primary, removable) = provider_removal_gateway(&root);
        let before = gateway
            .state
            .lock()
            .await
            .config
            .lock()
            .expect("gateway config")
            .clone();

        let error = gateway
            .remove_provider(primary.instance)
            .await
            .expect_err("the primary default must remain configured");

        assert_eq!(error.code, "invalid_config");
        assert_eq!(
            *gateway
                .state
                .lock()
                .await
                .config
                .lock()
                .expect("gateway config"),
            before
        );
        assert_eq!(
            credentials
                .get(
                    &removable.instance,
                    &removable.provider,
                    removable.base_url.as_deref(),
                )
                .expect("credential"),
            Some("unused-secret".into())
        );
    }

    #[tokio::test]
    async fn provider_removal_reloads_idle_chat_and_deletes_credential() {
        let root = tempfile::tempdir().expect("root");
        let workspace = root.path().join("workspace");
        std::fs::create_dir(&workspace).expect("workspace");
        let (gateway, credentials, primary, removable) = provider_removal_gateway(&root);
        let host = gateway.create_session(&workspace).await.expect("chat");
        let before = host.snapshot(None).await.expect("snapshot").ready.config;
        let mut composition = before.config;
        composition.provider = removable.clone();
        host.configure(before.revision, composition)
            .await
            .expect("select removable provider");
        let mut events = host.subscribe();
        let mut gateway_events = gateway.subscribe();
        let host_id = host.session_id().to_owned();

        let ready = gateway
            .remove_provider(removable.instance.clone())
            .await
            .expect("remove unused provider");

        assert!(host.is_alive());
        assert!(Arc::ptr_eq(
            &gateway.state.lock().await.sessions[&host_id].inner,
            &host.inner
        ));
        let config = host
            .snapshot(None)
            .await
            .expect("reloaded chat")
            .ready
            .config;
        assert_eq!(config.config.provider, primary);
        assert_eq!(
            config.config.middleware.setting("subagents", "model_route"),
            None
        );
        assert!(
            ready
                .provider_instances
                .iter()
                .all(|provider| provider.selection.instance != removable.instance)
        );
        assert_eq!(
            ready
                .default_config
                .as_ref()
                .expect("gateway default")
                .config
                .middleware
                .setting("subagents", "model_route"),
            None
        );
        assert_eq!(
            credentials
                .get(
                    &removable.instance,
                    &removable.provider,
                    removable.base_url.as_deref(),
                )
                .expect("credential"),
            None
        );
        assert!(matches!(
            events.recv().await.expect("reload event").message,
            ServerMessage::SessionChanged { .. }
        ));
        assert!(matches!(
            gateway_events.recv().await.expect("ready event").message,
            ServerMessage::Ready { payload } if payload == ready
        ));
    }

    #[tokio::test]
    async fn active_chat_blocks_provider_removal_without_changes() {
        let root = tempfile::tempdir().expect("root");
        let (gateway, credentials, _, removable) = provider_removal_gateway(&root);
        let before = gateway
            .state
            .lock()
            .await
            .config
            .lock()
            .expect("gateway config")
            .clone();
        let (commands, mut receiver) = mpsc::channel(1);
        let busy_selection = removable.clone();
        tokio::spawn(async move {
            if let Some(HostCommand::ProviderCutoverStatus { reply }) = receiver.recv().await {
                let _ = reply.send(ProviderCutoverStatus {
                    selection: busy_selection,
                    provider_epoch: 0,
                    idle: false,
                });
            }
        });
        let (events, _) = broadcast::channel(1);
        gateway.state.lock().await.sessions.insert(
            "busy".into(),
            HostHandle {
                inner: Arc::new(HostInner {
                    session_id: "busy".into(),
                    commands,
                    events,
                    accepts_file_attachments: Arc::new(AtomicBool::new(false)),
                    alive: Arc::new(AtomicBool::new(true)),
                }),
            },
        );

        let error = gateway
            .remove_provider(removable.instance.clone())
            .await
            .expect_err("active chat must block provider removal");

        assert_eq!(error.code, "agent_busy");
        assert_eq!(
            *gateway
                .state
                .lock()
                .await
                .config
                .lock()
                .expect("gateway config"),
            before
        );
        assert_eq!(
            credentials
                .get(
                    &removable.instance,
                    &removable.provider,
                    removable.base_url.as_deref(),
                )
                .expect("credential"),
            Some("unused-secret".into())
        );
    }

    #[tokio::test]
    async fn dormant_chat_falls_back_when_removed_provider_is_reopened() {
        let root = tempfile::tempdir().expect("root");
        let workspace = root.path().join("workspace");
        std::fs::create_dir(&workspace).expect("workspace");
        let (gateway, _, primary, removable) = provider_removal_gateway(&root);
        let host = gateway.create_session(&workspace).await.expect("chat");
        let before = host.snapshot(None).await.expect("snapshot").ready.config;
        let mut composition = before.config;
        composition.provider = removable.clone();
        host.configure(before.revision, composition)
            .await
            .expect("select removable provider");
        let session_id = host.session_id().to_owned();
        assert!(host.stop_if_idle().await);
        while host.is_alive() {
            tokio::task::yield_now().await;
        }
        gateway.state.lock().await.sessions.remove(&session_id);

        gateway
            .remove_provider(removable.instance)
            .await
            .expect("remove dormant provider");
        let reopened = gateway
            .open_session(&session_id)
            .await
            .expect("reopen chat");
        let config = reopened
            .snapshot(None)
            .await
            .expect("snapshot")
            .ready
            .config;

        assert_eq!(config.config.provider, primary);
        assert_eq!(
            config.config.middleware.setting("subagents", "model_route"),
            None
        );
    }

    #[tokio::test]
    async fn provider_removal_save_failure_keeps_resident_alive() {
        let root = tempfile::tempdir().expect("root");
        let workspace = root.path().join("workspace");
        std::fs::create_dir(&workspace).expect("workspace");
        let (gateway, credentials, _, removable) = provider_removal_gateway(&root);
        let host = gateway.create_session(&workspace).await.expect("chat");
        let session_id = host.session_id().to_owned();
        let config_path = root.path().join("state").join("gateway.toml");
        std::fs::remove_file(&config_path).expect("remove gateway config");
        std::fs::create_dir(&config_path).expect("block gateway config save");

        gateway
            .remove_provider(removable.instance.clone())
            .await
            .expect_err("gateway config save must fail");

        assert!(host.is_alive());
        assert!(Arc::ptr_eq(
            &gateway.state.lock().await.sessions[&session_id].inner,
            &host.inner
        ));
        assert!(
            gateway
                .state
                .lock()
                .await
                .config
                .lock()
                .expect("gateway config")
                .configured_providers
                .contains_key(&removable.instance)
        );
        assert_eq!(
            credentials
                .get(
                    &removable.instance,
                    &removable.provider,
                    removable.base_url.as_deref(),
                )
                .expect("credential"),
            Some("unused-secret".into())
        );
    }

    #[tokio::test]
    async fn provider_registration_commits_against_the_latest_usage() {
        let root = tempfile::tempdir().expect("root");
        let state_dir = root.path().join("state");
        let listen = "127.0.0.1:8741".parse().expect("listen address");
        let (store, config) =
            ConfigStore::initialize(state_dir.clone(), listen, None).expect("config");
        let credentials =
            Arc::new(CredentialStore::open(store.credentials_path()).expect("credentials"));
        let cron = Arc::new(CronStore::open(store.state_dir()).expect("cron"));
        let gateway = GatewayHost::start(store, config, credentials, cron).expect("gateway");
        let selection = ProviderConfig {
            instance: "openrouter".into(),
            provider: "openrouter".into(),
            model: "openai/gpt-5".into(),
            base_url: Some("https://connector.example/v1".into()),
            endpoint_auth: crate::wire::ProviderEndpointAuth::Credentialless,
            reasoning_effort: None,
            web_search: mobius::backend::model::provider::HostedWebSearch::Off,
        };
        let usage = mobius::protocol::TokenUsage {
            input_tokens: 13,
            total_tokens: 13,
            ..mobius::protocol::TokenUsage::default()
        };
        let state = gateway.state.lock().await;
        let stale = state.config.lock().expect("gateway config").clone();
        provider_registration(
            &stale,
            &selection,
            "Test",
            Default::default(),
            std::slice::from_ref(&selection.model),
            &[],
            true,
        )
        .expect("stale registration plan");
        {
            let mut latest = state.config.lock().expect("gateway config");
            assert!(
                latest
                    .observe_usage("openrouter", &usage)
                    .expect("observe usage")
            );
            state.store.save(&latest).expect("persist usage");
        }

        commit_provider_registration(
            &state,
            &selection,
            "Test",
            Default::default(),
            std::slice::from_ref(&selection.model),
            &[],
            true,
        )
        .expect("commit registration");

        let latest = state.config.lock().expect("gateway config").clone();
        assert_eq!(latest.profile().daily_usage[0].usage, usage);
        drop(state);
        assert_eq!(
            ConfigStore::open(state_dir)
                .expect("persisted gateway")
                .1
                .profile()
                .daily_usage[0]
                .usage,
            usage
        );
    }

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
                "responses".into(),
                "custom-secret".into(),
                Some(custom_endpoint.into()),
            )
            .await
            .expect("store custom credential");
        let error = gateway
            .set_credential(
                "openai_socket".into(),
                "openai_socket".into(),
                "fixed-secret".into(),
                Some(custom_endpoint.into()),
            )
            .await
            .expect_err("fixed provider endpoint must be rejected");

        assert_eq!(
            credentials
                .get("responses", "responses", Some(custom_endpoint))
                .expect("custom credential"),
            Some("custom-secret".into())
        );
        assert_eq!(
            credentials
                .get("responses", "openrouter", Some(custom_endpoint))
                .expect("different provider"),
            None
        );
        assert_eq!(error.code, "invalid_config");
        assert!(error.message.contains("fixed API endpoint"));
        assert_eq!(
            credentials
                .get("openai_socket", "openai_socket", None)
                .expect("fixed credential"),
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
            .set(
                "kimi",
                "kimi",
                "old-secret",
                Some("https://api.moonshot.ai/v1"),
            )
            .expect("initial Kimi credential");
        let cron = Arc::new(CronStore::open(store.state_dir()).expect("cron"));
        let gateway = GatewayHost::start(store, config, credentials, cron).expect("gateway");
        gateway
            .register_provider(
                ProviderConfig {
                    instance: "kimi".into(),
                    provider: "kimi".into(),
                    model: "kimi-k3".into(),
                    base_url: Some("https://api.moonshot.ai/v1".into()),
                    endpoint_auth: crate::wire::ProviderEndpointAuth::ProviderDefault,
                    reasoning_effort: Some("max".into()),
                    web_search: mobius::backend::model::provider::HostedWebSearch::Off,
                },
                "Test".into(),
                Default::default(),
                Vec::new(),
                Vec::new(),
                false,
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
            .set_credential(
                "kimi".into(),
                "kimi".into(),
                "new-secret".into(),
                Some("https://api.moonshot.ai/v1".into()),
            )
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
    fn credential_refresh_separates_instances_but_shares_a_browser_login() {
        let selection = ProviderConfig {
            instance: "responses-work".into(),
            provider: "responses".into(),
            model: "custom-model".into(),
            base_url: Some("https://first.example/v1".into()),
            endpoint_auth: crate::wire::ProviderEndpointAuth::ProviderDefault,
            reasoning_effort: None,
            web_search: mobius::backend::model::provider::HostedWebSearch::Off,
        };

        // An API key belongs to one instance; a sibling instance keeps its own.
        assert!(
            provider_refresh_matches(
                &selection,
                &ProviderRefresh::Instance {
                    instance: "responses-work".into(),
                    base_url: Some("https://first.example/v1".into()),
                }
            )
            .expect("matching instance and endpoint")
        );
        assert!(
            !provider_refresh_matches(
                &selection,
                &ProviderRefresh::Instance {
                    instance: "responses-personal".into(),
                    base_url: Some("https://first.example/v1".into()),
                }
            )
            .expect("different instance")
        );
        assert!(
            !provider_refresh_matches(
                &selection,
                &ProviderRefresh::Instance {
                    instance: "responses-work".into(),
                    base_url: Some("https://second.example/v1".into()),
                }
            )
            .expect("different endpoint")
        );

        // A browser login is stored per provider, so every instance of it refreshes.
        assert!(
            provider_refresh_matches(&selection, &ProviderRefresh::Provider("responses".into()))
                .expect("matching provider")
        );
        assert!(
            !provider_refresh_matches(&selection, &ProviderRefresh::Provider("anthropic".into()))
                .expect("different provider")
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
