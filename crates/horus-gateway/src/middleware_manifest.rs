//! Gateway-owned middleware catalog and configuration policy.

use std::collections::BTreeSet;

use crate::wire::{MiddlewareConfig, MiddlewareFeature};
use crate::{Error, Result};

#[derive(Clone, Copy)]
pub(crate) enum BuiltinMiddleware {
    Tools,
    Instructions,
    Cron,
    Skills,
    Subagents,
    Steering,
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
}

pub(crate) const MIDDLEWARE: [MiddlewareDefinition; 8] = [
    MiddlewareDefinition {
        kind: BuiltinMiddleware::Tools,
        id: "tools",
        label: "Tools",
        description: "Read and modify workspace files and run sandboxed commands",
        required: false,
        default_enabled: true,
    },
    MiddlewareDefinition {
        kind: BuiltinMiddleware::Instructions,
        id: "instructions",
        label: "Workspace instructions",
        description: "Load optional root AGENTS.md guidance",
        required: false,
        default_enabled: true,
    },
    MiddlewareDefinition {
        kind: BuiltinMiddleware::Cron,
        id: "cron",
        label: "Scheduling",
        description: "Schedule recurring agent work; always available",
        required: true,
        default_enabled: true,
    },
    MiddlewareDefinition {
        kind: BuiltinMiddleware::Skills,
        id: "skills",
        label: "Skills",
        description: "Discover local SKILL.md capabilities",
        required: false,
        default_enabled: true,
    },
    MiddlewareDefinition {
        kind: BuiltinMiddleware::Subagents,
        id: "subagents",
        label: "Subagents",
        description: "Run independent work asynchronously",
        required: false,
        default_enabled: true,
    },
    MiddlewareDefinition {
        kind: BuiltinMiddleware::Steering,
        id: "steering",
        label: "Steering",
        description: "Accept guidance during an active turn",
        required: false,
        default_enabled: true,
    },
    MiddlewareDefinition {
        kind: BuiltinMiddleware::Compaction,
        id: "compaction",
        label: "Compaction",
        description: "Compact long conversations as context fills",
        required: false,
        default_enabled: true,
    },
    MiddlewareDefinition {
        kind: BuiltinMiddleware::Sessions,
        id: "sessions",
        label: "Sessions",
        description: "Resume and fork durable chats; always available",
        required: true,
        default_enabled: true,
    },
];

pub(crate) fn features() -> Vec<MiddlewareFeature> {
    MIDDLEWARE
        .iter()
        .map(|feature| MiddlewareFeature {
            id: feature.id.into(),
            label: feature.label.into(),
            description: feature.description.into(),
            required: feature.required,
        })
        .collect()
}

pub(crate) fn default_config() -> MiddlewareConfig {
    let mut config = MiddlewareConfig {
        enabled: BTreeSet::new(),
    };
    for feature in MIDDLEWARE.iter().filter(|feature| !feature.required) {
        config.set_enabled(feature.id, feature.default_enabled);
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
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn required_middleware_is_advertised_but_not_configurable() {
        let config = default_config();
        let features = features();

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
}
