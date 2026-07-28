use super::*;

#[test]
fn setup_config_keeps_credentials_context_and_compaction_default() {
    let mut state = SetupState::new(None, PathBuf::from("auth.json"), SetupMode::Full);
    state.credential = "sk-test-secret".into();
    state.model = state.provider().models().len();
    state.custom_model = "custom-model".into();
    state.custom_context = "123456".into();

    let config = state.config();
    let model = &config.models["default"];

    assert_eq!(model.api_key.as_deref(), Some("sk-test-secret"));
    assert_eq!(model.context_window, Some(123_456));
    assert!(config.middleware.iter().any(|middleware| {
        matches!(
            middleware,
            MiddlewareSettings::Compaction {
                at_tokens: DEFAULT_COMPACTION_TOKENS
            }
        )
    }));
}

#[test]
fn provider_setup_skips_agent_features_and_approvals() {
    let state = SetupState::new(None, PathBuf::from("auth.json"), SetupMode::Provider);

    let steps = state.steps();

    assert!(
        !steps.contains(&Step::Features)
            && !steps.contains(&Step::Approvals)
            && steps.last() == Some(&Step::Review)
    );
}

#[test]
fn credential_setup_finishes_after_the_credential() {
    let mut state = SetupState::new(None, PathBuf::from("auth.json"), SetupMode::Credential);
    state.credential = "sk-test-secret".into();

    assert!(state.steps() == [Step::Credential] && matches!(state.confirm(), Flow::Finish));
}

#[test]
fn setup_stores_approval_policy_in_the_sandbox() {
    for (selection, expected) in [
        (0, ApprovalPolicy::On),
        (1, ApprovalPolicy::Allow),
        (2, ApprovalPolicy::AllowNetwork),
    ] {
        let mut state = SetupState::new(None, PathBuf::from("auth.json"), SetupMode::Full);
        state.approvals = selection;

        assert_eq!(state.config().sandbox.approval, expected);
    }
}

#[test]
fn setup_stores_subagent_limits() {
    let state = SetupState::new(None, PathBuf::from("auth.json"), SetupMode::Full);

    assert!(state.config().middleware.iter().any(|middleware| {
        matches!(
            middleware,
            MiddlewareSettings::Subagents {
                max_concurrency: 21,
                max_agents: 64,
                ..
            }
        )
    }));
}
