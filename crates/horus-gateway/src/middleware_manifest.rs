//! Gateway-owned middleware catalog and configuration policy.

use std::collections::{BTreeMap, BTreeSet};

use horus::backend::model::ModelChoice;
use horus::middleware::context_offloading::{ContextOffloading, DEFAULT_STALE_AFTER_TOKENS};
use horus::protocol::{
    FrontendSetting, FrontendSettingKind, FrontendSettingOption, FrontendSettingValue,
    MiddlewareFeature,
};

use crate::wire::MiddlewareConfig;
use crate::{Error, Result};

const MAX_MODEL_ROUTE_BYTES: usize = 4 * 1024;

#[derive(Clone, Copy)]
pub(crate) enum BuiltinMiddleware {
    Tools,
    Instructions,
    Cron,
    Skills,
    Tasks,
    Subagents,
    Steering,
    ContextOffloading,
    Compaction,
    Sessions,
}

pub(crate) struct MiddlewareDefinition {
    pub(crate) kind: BuiltinMiddleware,
    pub(crate) id: &'static str,
    label: &'static str,
    description: &'static str,
    pub(crate) required: bool,
    default_enabled: bool,
    settings: &'static [SettingDefinition],
}

#[derive(Clone, Copy)]
enum SettingDefinition {
    Integer {
        id: &'static str,
        label: &'static str,
        description: &'static str,
        min: i64,
        max: Option<i64>,
        step: i64,
        default: i64,
    },
    Select {
        id: &'static str,
        label: &'static str,
        description: &'static str,
        options: fn(&[ModelChoice]) -> Vec<FrontendSettingOption>,
        unset_label: Option<&'static str>,
        max_bytes: usize,
    },
}

const NO_SETTINGS: &[SettingDefinition] = &[];
const SUBAGENT_SETTINGS: &[SettingDefinition] = &[SettingDefinition::Select {
    id: "model_route",
    label: "Default model route",
    description: "Used when a subagent launch does not choose a model route",
    options: model_route_options,
    unset_label: Some("Inherit parent"),
    max_bytes: MAX_MODEL_ROUTE_BYTES,
}];
const CONTEXT_OFFLOADING_SETTINGS: &[SettingDefinition] = &[SettingDefinition::Integer {
    id: "stale_after_tokens",
    label: "Stale after tokens",
    description: "Successful tool results older than this trailing window are masked",
    min: 1,
    max: None,
    step: 10_000,
    default: DEFAULT_STALE_AFTER_TOKENS,
}];

pub(crate) const MIDDLEWARE: [MiddlewareDefinition; 10] = [
    MiddlewareDefinition {
        kind: BuiltinMiddleware::Tools,
        id: "tools",
        label: "Tools",
        description: "Read and modify workspace files and run sandboxed commands",
        required: false,
        default_enabled: true,
        settings: NO_SETTINGS,
    },
    MiddlewareDefinition {
        kind: BuiltinMiddleware::Instructions,
        id: "instructions",
        label: "Workspace instructions",
        description: "Load optional root AGENTS.md guidance",
        required: false,
        default_enabled: true,
        settings: NO_SETTINGS,
    },
    MiddlewareDefinition {
        kind: BuiltinMiddleware::Cron,
        id: "cron",
        label: "Scheduling",
        description: "Schedule recurring agent work; always available",
        required: true,
        default_enabled: true,
        settings: NO_SETTINGS,
    },
    MiddlewareDefinition {
        kind: BuiltinMiddleware::Skills,
        id: "skills",
        label: "Skills",
        description: "Discover local SKILL.md capabilities",
        required: false,
        default_enabled: true,
        settings: NO_SETTINGS,
    },
    MiddlewareDefinition {
        kind: BuiltinMiddleware::Tasks,
        id: "tasks",
        label: "Tasks",
        description: "Maintain a durable todo list for multi-step work",
        required: false,
        default_enabled: false,
        settings: NO_SETTINGS,
    },
    MiddlewareDefinition {
        kind: BuiltinMiddleware::Subagents,
        id: "subagents",
        label: "Subagents",
        description: "Run independent work asynchronously",
        required: false,
        default_enabled: true,
        settings: SUBAGENT_SETTINGS,
    },
    MiddlewareDefinition {
        kind: BuiltinMiddleware::Steering,
        id: "steering",
        label: "Steering",
        description: "Accept guidance during an active turn",
        required: false,
        default_enabled: true,
        settings: NO_SETTINGS,
    },
    MiddlewareDefinition {
        kind: BuiltinMiddleware::ContextOffloading,
        id: "context_offloading",
        label: "Context offloading",
        description: "Mask stale successful tool output from active model context",
        required: false,
        default_enabled: true,
        settings: CONTEXT_OFFLOADING_SETTINGS,
    },
    MiddlewareDefinition {
        kind: BuiltinMiddleware::Compaction,
        id: "compaction",
        label: "Compaction",
        description: "Compact long conversations as context fills",
        required: false,
        default_enabled: true,
        settings: NO_SETTINGS,
    },
    MiddlewareDefinition {
        kind: BuiltinMiddleware::Sessions,
        id: "sessions",
        label: "Sessions",
        description: "Resume and fork durable chats; always available",
        required: true,
        default_enabled: true,
        settings: NO_SETTINGS,
    },
];

pub(crate) fn features(models: &[ModelChoice]) -> Vec<MiddlewareFeature> {
    MIDDLEWARE
        .iter()
        .map(|feature| MiddlewareFeature {
            id: feature.id.into(),
            label: feature.label.into(),
            description: feature.description.into(),
            required: feature.required,
            settings: feature
                .settings
                .iter()
                .map(|setting| setting.schema(models))
                .collect(),
        })
        .collect()
}

pub(crate) fn default_config() -> MiddlewareConfig {
    let mut config = MiddlewareConfig {
        enabled: BTreeSet::new(),
        settings: BTreeMap::new(),
    };
    for feature in &MIDDLEWARE {
        if !feature.required {
            config.set_enabled(feature.id, feature.default_enabled);
        }
        for setting in feature.settings {
            if let Some(value) = setting.default_value() {
                config.set_setting(feature.id, setting.id(), Some(value));
            }
        }
    }
    config
}

pub(crate) fn validate(config: &MiddlewareConfig) -> Result<()> {
    for id in config.entries() {
        let feature = MIDDLEWARE
            .iter()
            .find(|feature| feature.id == id)
            .ok_or_else(|| Error::Config(format!("unknown middleware `{id}`")))?;
        if feature.required {
            return Err(Error::Config(format!(
                "required middleware `{id}` cannot be configured"
            )));
        }
    }
    for (middleware_id, settings) in &config.settings {
        let feature = definition(middleware_id)?;
        if feature.settings.is_empty() {
            return Err(Error::Config(format!(
                "middleware `{middleware_id}` has no settings"
            )));
        }
        for setting_id in settings.keys() {
            if !feature
                .settings
                .iter()
                .any(|setting| setting.id() == setting_id)
            {
                return Err(Error::Config(format!(
                    "unknown setting `{middleware_id}.{setting_id}`"
                )));
            }
        }
    }
    for feature in &MIDDLEWARE {
        for setting in feature.settings {
            setting.validate(feature.id, config.setting(feature.id, setting.id()))?;
        }
    }
    ContextOffloading::new(integer_setting(
        config,
        "context_offloading",
        "stale_after_tokens",
    )?)?;
    Ok(())
}

pub(crate) fn validate_choices(config: &MiddlewareConfig, models: &[ModelChoice]) -> Result<()> {
    for feature in &MIDDLEWARE {
        for setting in feature.settings {
            let SettingDefinition::Select { id, options, .. } = setting else {
                continue;
            };
            let Some(FrontendSettingValue::String(value)) = config.setting(feature.id, id) else {
                continue;
            };
            if !(options)(models)
                .iter()
                .any(|option| option.value == *value)
            {
                return Err(Error::Config(format!(
                    "middleware setting `{}.{id}` is not an advertised choice",
                    feature.id
                )));
            }
        }
    }
    Ok(())
}

pub(crate) fn integer_setting(
    config: &MiddlewareConfig,
    middleware: &str,
    setting: &str,
) -> Result<i64> {
    match config.setting(middleware, setting) {
        Some(FrontendSettingValue::Integer(value)) => Ok(*value),
        Some(FrontendSettingValue::String(_)) => Err(setting_type(middleware, setting, "integer")),
        None => Err(Error::Config(format!(
            "missing middleware setting `{middleware}.{setting}`"
        ))),
    }
}

pub(crate) fn string_setting<'a>(
    config: &'a MiddlewareConfig,
    middleware: &str,
    setting: &str,
) -> Result<Option<&'a str>> {
    match config.setting(middleware, setting) {
        Some(FrontendSettingValue::String(value)) => Ok(Some(value)),
        Some(FrontendSettingValue::Integer(_)) => Err(setting_type(middleware, setting, "string")),
        None => Ok(None),
    }
}

fn definition(id: &str) -> Result<&'static MiddlewareDefinition> {
    MIDDLEWARE
        .iter()
        .find(|feature| feature.id == id)
        .ok_or_else(|| Error::Config(format!("unknown middleware `{id}`")))
}

fn setting_type(middleware: &str, setting: &str, expected: &str) -> Error {
    Error::Config(format!(
        "middleware setting `{middleware}.{setting}` must be {expected}"
    ))
}

impl SettingDefinition {
    fn id(self) -> &'static str {
        match self {
            Self::Integer { id, .. } | Self::Select { id, .. } => id,
        }
    }

    fn default_value(self) -> Option<FrontendSettingValue> {
        match self {
            Self::Integer { default, .. } => Some(FrontendSettingValue::Integer(default)),
            Self::Select { .. } => None,
        }
    }

    fn schema(self, models: &[ModelChoice]) -> FrontendSetting {
        match self {
            Self::Integer {
                id,
                label,
                description,
                min,
                max,
                step,
                ..
            } => FrontendSetting {
                id: id.into(),
                label: label.into(),
                description: description.into(),
                kind: FrontendSettingKind::Integer { min, max, step },
            },
            Self::Select {
                id,
                label,
                description,
                options,
                unset_label,
                ..
            } => FrontendSetting {
                id: id.into(),
                label: label.into(),
                description: description.into(),
                kind: FrontendSettingKind::Select {
                    options: options(models),
                    unset_label: unset_label.map(str::to_string),
                },
            },
        }
    }

    fn validate(self, middleware: &str, value: Option<&FrontendSettingValue>) -> Result<()> {
        match (self, value) {
            (Self::Integer { min, max, .. }, Some(FrontendSettingValue::Integer(value)))
                if *value >= min && max.is_none_or(|max| *value <= max) =>
            {
                Ok(())
            }
            (Self::Integer { id, .. }, Some(FrontendSettingValue::Integer(_))) => {
                Err(Error::Config(format!(
                    "middleware setting `{middleware}.{id}` is out of range"
                )))
            }
            (Self::Integer { id, .. }, Some(FrontendSettingValue::String(_))) => {
                Err(setting_type(middleware, id, "integer"))
            }
            (Self::Integer { id, .. }, None) => Err(Error::Config(format!(
                "missing middleware setting `{middleware}.{id}`"
            ))),
            (Self::Select { max_bytes, .. }, Some(FrontendSettingValue::String(value)))
                if !value.trim().is_empty() && value.len() <= max_bytes =>
            {
                Ok(())
            }
            (Self::Select { id, max_bytes, .. }, Some(FrontendSettingValue::String(_))) => {
                Err(Error::Config(format!(
                    "middleware setting `{middleware}.{id}` must be 1–{max_bytes} bytes"
                )))
            }
            (Self::Select { id, .. }, Some(FrontendSettingValue::Integer(_))) => {
                Err(setting_type(middleware, id, "string"))
            }
            (
                Self::Select {
                    unset_label: Some(_),
                    ..
                },
                None,
            ) => Ok(()),
            (Self::Select { id, .. }, None) => Err(Error::Config(format!(
                "missing middleware setting `{middleware}.{id}`"
            ))),
        }
    }
}

fn model_route_options(models: &[ModelChoice]) -> Vec<FrontendSettingOption> {
    models
        .iter()
        .map(|choice| FrontendSettingOption {
            value: choice.route.clone(),
            label: choice.reasoning_effort.as_ref().map_or_else(
                || choice.group.clone(),
                |effort| format!("{} · {effort}", choice.group),
            ),
            description: format!("{} · {}", choice.model, choice.route),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn required_middleware_is_advertised_but_not_configurable() {
        let config = default_config();
        let features = features(&[]);

        assert!(!config.enabled("tasks"));
        assert!(config.enabled("context_offloading"));
        assert_eq!(
            integer_setting(&config, "context_offloading", "stale_after_tokens")
                .expect("context setting"),
            DEFAULT_STALE_AFTER_TOKENS,
        );
        assert!(
            features
                .iter()
                .any(|feature| feature.id == "sessions" && feature.required)
        );
        assert!(!config.enabled("sessions"));

        let mut invalid = config;
        invalid.set_enabled("sessions", true);
        assert!(validate(&invalid).is_err());

        invalid.set_enabled("sessions", false);
        invalid.set_enabled("missing", true);
        assert!(validate(&invalid).is_err());

        invalid.set_enabled("missing", false);
        invalid.set_setting(
            "context_offloading",
            "stale_after_tokens",
            Some(FrontendSettingValue::Integer(0)),
        );
        assert!(validate(&invalid).is_err());
    }

    #[test]
    fn config_serializes_only_selected_optional_middleware() {
        let mut config = default_config();
        config.set_enabled("skills", false);

        let encoded = serde_json::to_value(config).expect("serialize middleware config");

        assert!(
            encoded["enabled"]
                .as_array()
                .is_some_and(|ids| ids.iter().all(|id| id != "skills"))
        );
    }

    #[test]
    fn feature_settings_advertise_exact_gateway_choices() {
        let models = [ModelChoice {
            route: "provider::model::high".into(),
            group: "Provider · Model".into(),
            model: "model".into(),
            reasoning_effort: Some("high".into()),
            context_window: Some(200_000),
        }];

        let subagents = features(&models)
            .into_iter()
            .find(|feature| feature.id == "subagents")
            .expect("subagent feature");
        let FrontendSettingKind::Select {
            options,
            unset_label,
        } = &subagents.settings[0].kind
        else {
            panic!("subagent route must be a select setting")
        };

        assert_eq!(unset_label.as_deref(), Some("Inherit parent"));
        assert_eq!(options[0].value, models[0].route);

        let mut config = default_config();
        config.set_setting(
            "subagents",
            "model_route",
            Some(FrontendSettingValue::String(models[0].route.clone())),
        );
        assert!(validate_choices(&config, &models).is_ok());
        assert!(validate_choices(&config, &[]).is_err());
    }

    #[test]
    fn config_rejects_unknown_and_mistyped_settings() {
        let mut config = default_config();
        config.set_setting("tools", "extra", Some(FrontendSettingValue::Integer(1)));
        assert!(validate(&config).is_err());

        config.set_setting("tools", "extra", None);
        config.set_setting(
            "context_offloading",
            "stale_after_tokens",
            Some(FrontendSettingValue::String("50000".into())),
        );
        assert!(validate(&config).is_err());
    }
}
