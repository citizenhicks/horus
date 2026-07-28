use super::*;
use horus::backend::checkpoint::SessionPageRequest;

const TEST_CONFIG: &str = r#"
[agent]
model = "primary"
system_prompt = "You are Horus."
context_window = 100000

[models.primary]
provider = "openai_socket"
model = "gpt-test"
api_key_env = "TEST_API_KEY"
reasoning_effort = "medium"
web_search = "off"

[[middleware]]
name = "tools"

[[middleware]]
name = "steering"

[[middleware]]
name = "compaction"
at_tokens = 80000

[[middleware]]
name = "sessions"

[sandbox]
command_timeout_seconds = 120
approval = "on"

[checkpoint]
path = "horus.sqlite3"
"#;

fn config() -> FileConfig {
    let mut config: FileConfig = toml::from_str(TEST_CONFIG).expect("parse manifest");
    config
        .models
        .get_mut("primary")
        .expect("primary model")
        .api_key = Some("test-key".into());
    config
}

#[test]
fn manifest_builds_declared_capabilities_and_model() {
    let config = config();
    config.validate().expect("validate manifest");
    let workspace = tempfile::tempdir().expect("workspace");
    let middleware = build_middleware(&config.middleware, workspace.path(), None)
        .expect("build middleware")
        .frontend()
        .expect("frontend catalog")
        .into_iter()
        .map(|contribution| contribution.capability)
        .collect::<Vec<_>>();
    let models =
        build_models(&config.models, &config.agent.model, workspace.path()).expect("build models");
    let choice = models.choices().next().expect("model choice");

    assert_eq!(middleware, ["tools", "steering", "sessions"]);
    assert_eq!(
        (choice.route.as_str(), choice.model.as_str()),
        ("primary", "gpt-test")
    );
}

#[cfg(unix)]
#[test]
fn saved_config_is_owner_only() {
    use std::os::unix::fs::PermissionsExt;

    let config = config();
    let directory = tempfile::tempdir().expect("temporary directory");
    let path = directory.path().join("config.toml");

    save_config(&path, &config, SaveMode::New).expect("save config");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644))
        .expect("make config permissive");
    save_config(&path, &config, SaveMode::Replace).expect("replace config");

    assert_eq!(
        std::fs::metadata(path)
            .expect("config metadata")
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
}

#[test]
fn default_state_lives_under_the_home_directory() {
    assert_eq!(
        resolve_state_dir(None, Some(PathBuf::from("/home/horus"))).expect("state directory"),
        Path::new("/home/horus/.horus")
    );
}

#[test]
fn parse_errors_do_not_echo_api_keys() {
    let error = parse_config(
        Path::new("config.toml"),
        "[models.primary]\napi_key = \"sk-test-secret\" trailing",
    )
    .err()
    .expect("reject invalid config");

    assert!(!error.to_string().contains("sk-test-secret"));
}

#[test]
fn provider_manifest_adds_reasoning_choices() {
    let source = TEST_CONFIG
        .replace("openai_socket", "kimi")
        .replace("gpt-test", "kimi-k3")
        .replace("TEST_API_KEY", "MOONSHOT_API_KEY")
        .replace(
            "reasoning_effort = \"medium\"",
            "reasoning_effort = \"high\"",
        );
    let mut config: FileConfig = toml::from_str(&source).expect("parse manifest");
    config
        .models
        .get_mut("primary")
        .expect("primary model")
        .api_key = Some("test-key".into());

    let router = build_models(&config.models, &config.agent.model, Path::new("."))
        .expect("build model choices");
    let choices = router
        .choices()
        .map(|choice| {
            (
                choice.route.as_str(),
                choice.reasoning_effort.as_deref(),
                choice.context_window,
            )
        })
        .collect::<Vec<_>>();

    assert_eq!(
        choices,
        [
            ("primary", Some("high"), Some(1_048_576)),
            (
                "__horus:primary:reasoning:low",
                Some("low"),
                Some(1_048_576)
            ),
            (
                "__horus:primary:reasoning:max",
                Some("max"),
                Some(1_048_576)
            ),
        ]
    );
}

#[tokio::test]
async fn launches_are_fresh_unless_a_session_is_explicit() {
    let workspace = tempfile::tempdir().expect("workspace");
    let state_dir = workspace.path().join("state");
    for _ in 0..2 {
        let agent = horus::agent::create_agent(
            config()
                .build(workspace.path(), &state_dir, None)
                .expect("build config")
                .config
                .clone(),
        )
        .await
        .expect("create agent");
        drop(agent);
    }
    let agent = horus::agent::create_agent(
        config()
            .build(workspace.path(), &state_dir, Some("explicit".into()))
            .expect("build resumed config")
            .config
            .clone(),
    )
    .await
    .expect("create resumed agent");
    drop(agent);

    let sessions = SqliteCheckpoint::new(state_dir.join("horus.sqlite3"))
        .expect("open checkpoint database")
        .list_sessions_page(SessionPageRequest {
            cursor: None,
            limit: 100,
        })
        .await
        .expect("list sessions")
        .sessions;

    assert_eq!(sessions.len(), 3);
    assert!(
        sessions
            .iter()
            .any(|session| session.session_id == "explicit")
    );
}
