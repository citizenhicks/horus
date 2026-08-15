use horus::backend::checkpoint::{Checkpoint, sqlite::SqliteCheckpoint};
use horus::backend::model::provider::HostedWebSearch;
use horus::protocol::FrontendSymbol;

use super::*;

#[test]
fn provider_status_uses_manifest_defaults() {
    let status = provider_status(provider("openai_socket").expect("provider"), false, None);

    assert_eq!(status.provider, "openai_socket");
    assert_eq!(status.label, "OpenAI");
    assert_eq!(status.symbol, FrontendSymbol::ChatGpt);
    assert_eq!(status.models[0].id, "gpt-5.6-sol");
    assert_eq!(
        status.default_api_key_env.as_deref(),
        Some("OPENAI_API_KEY")
    );
    assert_eq!(
        status.models[0].default_reasoning.as_deref(),
        Some("medium")
    );
    assert_eq!(status.web_search[0], HostedWebSearch::Off);

    let custom = provider_status(provider("responses").expect("provider"), false, None);
    assert!(custom.models.is_empty());
    assert!(custom.model_ids_configurable);
    assert!(custom.model_ids.is_empty());
    assert!(custom.reasoning_efforts.is_empty());
    assert_eq!(
        custom.default_base_url.as_deref(),
        Some("https://api.openai.com/v1")
    );
    assert_eq!(custom.default_api_key_env, None);

    let openrouter = provider_status(provider("openrouter").expect("provider"), false, None);
    assert!(openrouter.models.is_empty());
    assert!(openrouter.model_ids_configurable);
    assert_eq!(openrouter.default_base_url, None);
}

#[test]
fn configured_catalog_resolves_manifest_and_opaque_custom_routes() {
    let root = tempfile::tempdir().expect("root");
    let state = root.path().join("state");
    let (store, config) = ConfigStore::initialize(
        state,
        "127.0.0.1:8741".parse().expect("listen address"),
        None,
    )
    .expect("config");
    let credentials = CredentialStore::open(store.credentials_path()).expect("credential store");
    credentials
        .set("kimi", "kimi-secret", None)
        .expect("Kimi credential");
    credentials
        .set("responses", "custom-secret", Some("https://example.com/v1"))
        .expect("custom credential");
    let kimi = ProviderConfig {
        provider: "kimi".into(),
        model: "kimi-k3".into(),
        base_url: None,
        reasoning_effort: Some("max".into()),
        web_search: HostedWebSearch::Off,
    };
    let custom = ProviderConfig {
        provider: "responses".into(),
        model: "vendor/model-opaque".into(),
        base_url: Some("https://example.com/v1".into()),
        reasoning_effort: Some("provider-defined".into()),
        web_search: HostedWebSearch::Off,
    };
    let alternate_model = "vendor/model-alternate".to_string();
    let config = config
        .registering_provider(kimi, Vec::new(), Vec::new())
        .and_then(|config| {
            config.registering_provider(
                custom.clone(),
                vec![custom.model.clone(), alternate_model.clone()],
                vec!["provider-defined".into(), "minimal".into()],
            )
        })
        .expect("register providers");

    let choices = configured_model_choices(&config, &store, &credentials).expect("catalog");
    let custom_route = choices
        .iter()
        .find(|choice| choice.model == custom.model)
        .expect("custom choice");
    let resolved =
        configured_provider_for_route(&config, &store, &credentials, &custom_route.route)
            .expect("resolve custom route");
    let model_providers =
        configured_model_providers(&config, &store, &credentials).expect("provider IDs");

    assert!(
        choices
            .first()
            .is_some_and(|choice| choice.route.starts_with("kimi::"))
    );
    assert_eq!(resolved, custom);
    assert_eq!(model_providers[&custom_route.route], "responses");
    assert!(choices.iter().any(|choice| choice.model == alternate_model));
    assert!(choices.iter().any(|choice| {
        choice.model == alternate_model && choice.reasoning_effort.as_deref() == Some("minimal")
    }));
    assert_eq!(
        custom_route.group,
        format!(
            "{} · {}",
            provider("responses").expect("provider").label(),
            custom.model
        )
    );
}

#[test]
fn usage_sink_attributes_a_model_route_to_its_provider() {
    let root = tempfile::tempdir().expect("root");
    let state = root.path().join("state");
    let (store, config) = ConfigStore::initialize(
        state.clone(),
        "127.0.0.1:8741".parse().expect("listen address"),
        None,
    )
    .expect("config");
    let gateway = Mutex::new(config);
    let model_providers = BTreeMap::from([("primary".into(), "openai_socket".into())]);
    let usage = TokenUsage {
        input_tokens: 11,
        total_tokens: 11,
        ..TokenUsage::default()
    };

    persist_usage(&gateway, &store, &model_providers, "primary", &usage).expect("persist usage");

    let (_, restored) = ConfigStore::open(state).expect("reopen config");
    let daily_usage = restored.profile().daily_usage;
    assert_eq!(daily_usage.len(), 1);
    assert_eq!(daily_usage[0].provider, "openai_socket");
    assert_eq!(daily_usage[0].usage, usage);
}

#[test]
fn custom_selection_without_reasoning_uses_the_first_configured_effort() {
    let root = tempfile::tempdir().expect("root");
    let (store, config) = ConfigStore::initialize(
        root.path().join("state"),
        "127.0.0.1:8741".parse().expect("listen address"),
        None,
    )
    .expect("config");
    let credentials = CredentialStore::open(store.credentials_path()).expect("credential store");
    credentials
        .set(
            "responses",
            "custom-secret",
            Some("http://127.0.0.1:11434/v1"),
        )
        .expect("custom credential");
    let selection = ProviderConfig {
        provider: "responses".into(),
        model: "local-model".into(),
        base_url: Some("http://127.0.0.1:11434/v1".into()),
        reasoning_effort: None,
        web_search: HostedWebSearch::Off,
    };
    let config = config
        .registering_provider(
            selection.clone(),
            vec![selection.model.clone()],
            vec!["high".into(), "medium".into()],
        )
        .expect("register provider");

    let choices = configured_model_choices(&config, &store, &credentials).expect("catalog");
    let (router, _) =
        build_models(&config, &selection, &store, &credentials).expect("build selected model");
    let selected = router.choices().next().expect("selected route");

    assert_eq!(choices[0].reasoning_effort.as_deref(), Some("high"));
    assert_eq!(selected.reasoning_effort.as_deref(), Some("high"));
    assert_eq!(router.default_provider(), choices[0].route);
}

#[test]
fn custom_responses_requires_an_endpoint_bound_stored_credential() {
    let root = tempfile::tempdir().expect("root");
    let state = root.path().join("state");
    let (store, _) = ConfigStore::initialize(
        state,
        "127.0.0.1:8741".parse().expect("listen address"),
        None,
    )
    .expect("config");
    let credentials = CredentialStore::open(store.credentials_path()).expect("credential store");
    let selection = ProviderConfig {
        provider: "responses".into(),
        model: "custom-model".into(),
        base_url: Some("https://example.com/v1".into()),
        reasoning_effort: None,
        web_search: HostedWebSearch::Off,
    };

    let error = resolve_credential(
        provider("responses").expect("provider"),
        selection.base_url.as_deref(),
        &store,
        &credentials,
    )
    .err()
    .expect("custom provider must require stored credentials");

    assert!(
        error
            .to_string()
            .contains("set a credential for `responses`")
    );

    credentials
        .set(
            "responses",
            "official-secret",
            Some("https://api.openai.com/v1"),
        )
        .expect("store endpoint-bound credential");
    assert!(
        resolve_credential(
            provider("responses").expect("provider"),
            selection.base_url.as_deref(),
            &store,
            &credentials,
        )
        .is_err()
    );
}

#[tokio::test]
async fn updating_the_chat_recipe_preserves_capability_metadata() {
    let root = tempfile::tempdir().expect("root");
    let workspace = root.path().join("workspace");
    std::fs::create_dir(&workspace).expect("workspace");
    let (store, gateway) = ConfigStore::initialize(
        root.path().join("state"),
        "127.0.0.1:8741".parse().expect("listen address"),
        None,
    )
    .expect("config");
    let gateway = gateway
        .registering_provider(
            crate::wire::AgentComposition::default().provider,
            Vec::new(),
            Vec::new(),
        )
        .expect("register provider");
    let credentials =
        Arc::new(CredentialStore::open(store.credentials_path()).expect("credentials"));
    let cron = Arc::new(CronStore::open(store.state_dir()).expect("cron"));
    let checkpoints: Arc<dyn CheckpointStore> =
        Arc::new(SqliteCheckpoint::new(store.checkpoints_path()).expect("checkpoints"));
    let original = ChatSpec::new(
        &workspace,
        crate::wire::VersionedAgentConfig {
            revision: 1,
            config: crate::wire::AgentComposition::default(),
        },
        store.state_dir(),
        None,
    )
    .expect("chat spec");
    let mut checkpoint = Checkpoint::empty("chat");
    checkpoint.metadata = original.metadata().expect("chat metadata");
    checkpoint.metadata.insert(
        "capability.test".into(),
        serde_json::json!({"identity": "preserved"}),
    );
    checkpoints
        .save(&checkpoint, &[], None)
        .await
        .expect("seed checkpoint");
    let (reusable_router, _) = unavailable_models(&gateway, &original.agent.config.provider)
        .expect("unavailable model router");
    let mut composition = original.agent.config.clone();
    composition.middleware.set_enabled("cron", false);
    composition.middleware.set_enabled("scratchpad", false);
    composition.system_prompt = "updated instructions".into();
    let updated = original
        .replacing_agent(1, composition, &gateway, store.state_dir(), None)
        .expect("updated chat spec");
    let gateway = Arc::new(Mutex::new(gateway));

    let built = assemble(
        gateway,
        &updated,
        &store,
        credentials,
        cron,
        Arc::clone(&checkpoints),
        ScratchpadStore::new(Arc::clone(&checkpoints)),
        SessionFileStore::new(store.state_dir()),
        Some("chat".into()),
        "test",
        true,
        Some(Arc::clone(&reusable_router)),
    )
    .await
    .expect("assemble chat");
    assert!(Arc::ptr_eq(&reusable_router, &built.model_router));
    let scratchpad = built
        .agent
        .frontend()
        .contributions()
        .iter()
        .find(|contribution| contribution.capability == "scratchpad")
        .expect("disabled scratchpad management surface");
    assert_eq!(scratchpad.commands.len(), 1);
    assert_eq!(scratchpad.commands[0].name, "scratchpad");
    assert_eq!(scratchpad.widgets.len(), 2);
    let (sender, mut events) = built.agent.into_parts();
    drop(sender);
    while events.recv().await.is_some() {}
    let checkpoint = checkpoints
        .load("chat")
        .await
        .expect("load checkpoint")
        .expect("saved checkpoint");
    let saved = ChatSpec::from_metadata(&checkpoint.metadata, store.state_dir(), None)
        .expect("saved chat spec");

    assert_eq!(
        checkpoint.metadata["capability.test"],
        serde_json::json!({"identity": "preserved"})
    );
    assert_eq!(saved.agent.revision, 2);
    assert!(!saved.agent.config.middleware.enabled("cron"));
    assert_eq!(saved.agent.config.system_prompt, "updated instructions");
}
