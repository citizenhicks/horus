//! Local SKILL.md discovery and lazy loading.

use std::collections::BTreeMap;
use std::env;
use std::io::Read as _;
use std::path::Component;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;

use cap_std::ambient_authority;
use cap_std::fs::Dir;
use serde::Deserialize;
use serde_json::Value;

use super::Middleware;
use super::PromptSection;
use super::manifest::MiddlewareManifest;
use super::tools::Catalog;
use super::tools::ExecutionMode;
use super::tools::Tool;
use super::tools::ToolContext;
use super::tools::labeled_tool_heading;
use super::tools::render_tool_event;
use crate::BoxFuture;
use crate::Error;
use crate::Result;
use crate::backend::model::ToolDefinition;
use crate::protocol::EventMsg;
use crate::protocol::FrontendBlock;
use crate::protocol::FrontendContribution;
use crate::protocol::FrontendReference;
use crate::protocol::FrontendSlot;
use crate::protocol::FrontendTone;
use crate::protocol::FrontendWidget;

mod text {
    include!(concat!(env!("OUT_DIR"), "/src_middleware_skills_text.rs"));
}

const MAX_SKILLS: usize = 64;
const MAX_SKILL_BYTES: u64 = 40_000;
const MAX_SKILL_PATH_BYTES: usize = 4_096;
const SKILL_FILE: &str = "SKILL.md";
/// Configuration and presentation metadata for local skills.
pub const MANIFEST: MiddlewareManifest = MiddlewareManifest {
    id: "skills",
    label: text::MANIFEST_LABEL,
    description: text::MANIFEST_DESCRIPTION,
    required: false,
    default_enabled: true,
    settings: &[],
};

#[derive(Clone)]
struct Skill {
    name: String,
    description: String,
    directory: Arc<Dir>,
}

/// Discovers bounded skill metadata and contributes a lazy `load_skill` tool.
pub struct Skills {
    skills: BTreeMap<String, Skill>,
    prompt: String,
}

impl Skills {
    /// Discovers direct child `SKILL.md` files under each root.
    pub fn discover(roots: impl IntoIterator<Item = PathBuf>) -> Result<Self> {
        let mut skills = BTreeMap::new();
        discover_roots(roots, &mut skills, false)?;
        Ok(Self {
            skills,
            prompt: text::PROMPT_DEFAULT.into(),
        })
    }

    /// Adds user-installed skills after the explicit roots.
    pub fn discover_installed(roots: impl IntoIterator<Item = PathBuf>) -> Result<Self> {
        let mut discovered = Self::discover(roots)?;
        discover_roots(installed_skill_roots(), &mut discovered.skills, true)?;
        Ok(discovered)
    }

    /// Overrides the instruction placed before discovered skill metadata.
    pub fn prompt(mut self, prompt: impl Into<String>) -> Result<Self> {
        let prompt = prompt.into();
        if prompt.trim().is_empty() {
            return Err(Error::Config("skills prompt cannot be empty".into()));
        }
        self.prompt = prompt;
        Ok(self)
    }

    fn section(&self) -> Option<PromptSection> {
        if self.skills.is_empty() {
            return None;
        }
        let skills = self
            .skills
            .values()
            .map(|skill| format!("- {}: {}", skill.name, skill.description))
            .collect::<Vec<_>>()
            .join("\n");
        Some(PromptSection::new(format!(
            "{}\n\n{skills}",
            self.prompt.trim()
        )))
    }
}

fn discover_roots(
    roots: impl IntoIterator<Item = PathBuf>,
    skills: &mut BTreeMap<String, Skill>,
    keep_existing: bool,
) -> Result<()> {
    for root in roots {
        let root = match Dir::open_ambient_dir(root, ambient_authority()) {
            Ok(root) => root,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        let mut directories = collect_skill_directories(
            root.entries()?
                .map(|entry| entry.map(|entry| PathBuf::from(entry.file_name()))),
        )?;
        directories.sort();
        for directory_path in directories {
            let directory = match root.open_dir(&directory_path) {
                Ok(directory) => directory,
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
                    ) =>
                {
                    continue;
                }
                Err(error) => return Err(error.into()),
            };
            let metadata = match directory.metadata(SKILL_FILE) {
                Ok(metadata) => metadata,
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
                    ) =>
                {
                    continue;
                }
                Err(error) => return Err(error.into()),
            };
            if !metadata.is_file() {
                continue;
            }
            let content = read_skill_resource(&directory, Path::new(SKILL_FILE))?;
            let skill_path = directory_path.join(SKILL_FILE);
            let (name, description) = skill_metadata(&skill_path, &content);
            if skills.contains_key(&name) {
                if keep_existing {
                    continue;
                }
                return Err(Error::Duplicate(format!("skill `{name}`")));
            }
            if skills.len() == MAX_SKILLS {
                return Err(Error::Config(format!("skill count exceeds {MAX_SKILLS}")));
            }
            skills.insert(
                name.clone(),
                Skill {
                    name,
                    description,
                    directory: Arc::new(directory),
                },
            );
        }
    }
    Ok(())
}

fn collect_skill_directories(
    entries: impl Iterator<Item = std::io::Result<PathBuf>>,
) -> std::io::Result<Vec<PathBuf>> {
    entries.collect()
}

fn installed_skill_roots() -> Vec<PathBuf> {
    let home = env::var_os("HOME")
        .or_else(|| env::var_os("USERPROFILE"))
        .map(PathBuf::from);
    let codex_home = env::var_os("CODEX_HOME").map(PathBuf::from);
    installed_skill_roots_from(home, codex_home)
}

fn installed_skill_roots_from(home: Option<PathBuf>, codex_home: Option<PathBuf>) -> Vec<PathBuf> {
    let codex_home = codex_home.or_else(|| home.as_ref().map(|path| path.join(".codex")));
    let mut roots = Vec::new();
    if let Some(home) = home {
        roots.push(home.join(".agents/skills"));
    }
    if let Some(codex_home) = codex_home {
        roots.push(codex_home.join("skills"));
        roots.push(codex_home.join("skills/.system"));
    }
    #[cfg(unix)]
    roots.push(PathBuf::from("/etc/codex/skills"));
    roots
}

impl Middleware for Skills {
    fn name(&self) -> &'static str {
        MANIFEST.id
    }

    fn register(&self, catalog: &mut Catalog, _runtime: &super::RuntimeContext) -> Result<()> {
        catalog.register(Arc::new(LoadSkill {
            skills: self.skills.clone(),
        }))
    }

    fn prompt_section(&self, _runtime: &super::RuntimeContext) -> Result<Option<PromptSection>> {
        Ok(self.section())
    }

    fn frontend(&self) -> FrontendContribution {
        FrontendContribution {
            capability: self.name().into(),
            accepts_file_attachments: false,
            count: Some(self.skills.len()),
            commands: Vec::new(),
            widgets: vec![FrontendWidget {
                id: "count".into(),
                slot: FrontendSlot::Header,
                text: format!("skills {}", self.skills.len()),
                tone: FrontendTone::Neutral,
                symbol: None,
                icon_only: false,
                progress: None,
                content: None,
                action: None,
            }],
            references: self
                .skills
                .values()
                .map(|skill| FrontendReference {
                    trigger: '$',
                    value: skill.name.clone(),
                    description: skill.description.clone(),
                })
                .collect(),
            active_input: None,
        }
    }

    fn render(&self, event: &EventMsg, _session_id: &str) -> Option<FrontendBlock> {
        render_tool_event(
            event,
            |name| name == "load_skill",
            |_, arguments| labeled_tool_heading(text::RENDER_SKILL, "name", arguments),
        )
    }
}

#[derive(Deserialize)]
struct LoadSkillArgs {
    name: String,
    path: Option<String>,
}

struct LoadSkill {
    skills: BTreeMap<String, Skill>,
}

impl Tool for LoadSkill {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "load_skill".into(),
            description: text::TOOL_LOAD_SKILL_DESCRIPTION.into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "name": {"type": "string"},
                    "path": {
                        "type": "string",
                        "minLength": 1,
                        "maxLength": MAX_SKILL_PATH_BYTES,
                        "description": text::TOOL_LOAD_SKILL_PARAMETER_PATH_DESCRIPTION
                    }
                },
                "required": ["name"],
                "additionalProperties": false
            }),
        }
    }

    fn execution_mode(&self) -> ExecutionMode {
        ExecutionMode::Parallel
    }

    fn call<'a>(
        &'a self,
        _context: ToolContext,
        arguments: Value,
    ) -> BoxFuture<'a, Result<String>> {
        Box::pin(async move {
            let arguments: LoadSkillArgs = serde_json::from_value(arguments)?;
            let skill = self
                .skills
                .get(&arguments.name)
                .ok_or_else(|| Error::Unknown(format!("skill `{}`", arguments.name)))?;
            let directory = Arc::clone(&skill.directory);
            let path = skill_resource_path(arguments.path.as_deref())?;
            tokio::task::spawn_blocking(move || read_skill_resource(&directory, &path))
                .await
                .map_err(|_| unavailable_skill_resource())?
        })
    }
}

fn skill_resource_path(path: Option<&str>) -> Result<PathBuf> {
    let path = path.unwrap_or(SKILL_FILE);
    if path.len() > MAX_SKILL_PATH_BYTES {
        return Err(unavailable_skill_resource());
    }
    Ok(PathBuf::from(path))
}

fn read_skill_resource(directory: &Dir, path: &Path) -> Result<String> {
    if path.as_os_str().is_empty()
        || path.is_absolute()
        || path
            .components()
            .any(|part| !matches!(part, Component::Normal(_) | Component::CurDir))
    {
        return Err(unavailable_skill_resource());
    }
    // Avoid blocking on static special files, then verify the opened handle again.
    if !directory
        .metadata(path)
        .map_err(|_| unavailable_skill_resource())?
        .is_file()
    {
        return Err(unavailable_skill_resource());
    }
    let file = directory
        .open(path)
        .map_err(|_| unavailable_skill_resource())?;
    if !file
        .metadata()
        .map_err(|_| unavailable_skill_resource())?
        .is_file()
    {
        return Err(unavailable_skill_resource());
    }
    let mut bytes = Vec::new();
    file.take(MAX_SKILL_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| unavailable_skill_resource())?;
    if bytes.len() as u64 > MAX_SKILL_BYTES {
        return Err(Error::Tool(format!(
            "skill resource exceeds {MAX_SKILL_BYTES} bytes"
        )));
    }
    String::from_utf8(bytes).map_err(|_| Error::Tool("skill resource is not valid UTF-8".into()))
}

fn unavailable_skill_resource() -> Error {
    Error::Tool("skill resource is unavailable".into())
}

fn skill_metadata(path: &std::path::Path, content: &str) -> (String, String) {
    let fallback = path
        .parent()
        .and_then(std::path::Path::file_name)
        .map_or_else(
            || "skill".into(),
            |name| name.to_string_lossy().into_owned(),
        );
    let mut name = None;
    let mut description = None;
    if content.starts_with("---\n")
        && let Some(frontmatter) = content[4..].split("\n---").next()
    {
        for line in frontmatter.lines() {
            if let Some(value) = line.strip_prefix("name:") {
                name = Some(unquote(value));
            } else if let Some(value) = line.strip_prefix("description:") {
                description = Some(unquote(value));
            }
        }
    }
    (
        name.filter(|value| !value.is_empty()).unwrap_or(fallback),
        description
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| text::FALLBACK_SKILL_DESCRIPTION.into())
            .chars()
            .take(500)
            .collect(),
    )
}

fn unquote(value: &str) -> String {
    value
        .trim()
        .trim_matches(|character| character == '"' || character == '\'')
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_section_is_absent_without_skills() {
        let temporary = tempfile::tempdir().expect("temporary skills");
        let skills = Skills::discover([temporary.path().to_path_buf()]).expect("empty skills");

        assert_eq!(skills.section(), None);
    }

    #[test]
    fn prompt_section_uses_markdown_in_skill_name_order() {
        let temporary = tempfile::tempdir().expect("temporary skills");
        let root = temporary.path().join("skills");
        write_skill(&root, "second", "zebra", "Last alphabetically");
        write_skill(&root, "first", "alpha", "First alphabetically");
        let skills = Skills::discover([root]).expect("skills");

        assert_eq!(
            skills.section(),
            Some(PromptSection::new(format!(
                "{}\n\n- alpha: First alphabetically\n- zebra: Last alphabetically",
                text::PROMPT_DEFAULT
            )))
        );
    }

    #[test]
    fn installed_skills_do_not_replace_explicit_skills() {
        let temporary = tempfile::tempdir().expect("temporary skills");
        let explicit = temporary.path().join("explicit");
        let installed = temporary.path().join("installed");
        write_skill(&explicit, "shared", "shared", "explicit");
        write_skill(&installed, "shared", "shared", "installed");
        write_skill(&installed, "global", "global", "installed");
        let mut discovered = Skills::discover([explicit]).expect("explicit skills");

        discover_roots([installed], &mut discovered.skills, true).expect("installed skills");

        assert_eq!(
            discovered
                .skills
                .iter()
                .map(|(name, skill)| (name.as_str(), skill.description.as_str()))
                .collect::<Vec<_>>(),
            vec![("global", "installed"), ("shared", "explicit")]
        );
    }

    #[test]
    fn directory_entry_errors_are_not_silently_ignored() {
        let entries = vec![
            Ok(PathBuf::from("valid")),
            Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "unreadable entry",
            )),
        ];

        assert_eq!(
            collect_skill_directories(entries.into_iter())
                .expect_err("entry error")
                .kind(),
            std::io::ErrorKind::PermissionDenied
        );
    }

    #[test]
    fn non_directory_entries_are_ignored() {
        let temporary = tempfile::tempdir().expect("temporary skills");
        let root = temporary.path().join("skills");
        write_skill(&root, "valid", "valid", "valid");
        std::fs::write(root.join(".installed"), "").expect("write marker");

        let discovered = Skills::discover([root]).expect("discover skills");

        assert_eq!(discovered.skills.keys().collect::<Vec<_>>(), vec!["valid"]);
    }

    #[test]
    fn skill_resource_rejects_parent_escape() {
        let temporary = tempfile::tempdir().expect("temporary skills");
        let skill = temporary.path().join("skill");
        std::fs::create_dir(&skill).expect("create skill");
        std::fs::write(temporary.path().join("outside.md"), "outside").expect("write outside");
        let directory = Dir::open_ambient_dir(&skill, ambient_authority()).expect("open skill");

        assert!(read_skill_resource(&directory, Path::new("../outside.md")).is_err());
    }

    #[test]
    fn skill_resource_rejects_oversized_path() {
        let path = "a".repeat(MAX_SKILL_PATH_BYTES + 1);

        assert!(skill_resource_path(Some(&path)).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn skill_resource_rejects_symlink_escape() {
        use std::os::unix::fs::symlink;

        let temporary = tempfile::tempdir().expect("temporary skills");
        let skill = temporary.path().join("skill");
        std::fs::create_dir(&skill).expect("create skill");
        let outside = temporary.path().join("outside.md");
        std::fs::write(&outside, "outside").expect("write outside");
        symlink(outside, skill.join("escape.md")).expect("create escape");
        let directory = Dir::open_ambient_dir(&skill, ambient_authority()).expect("open skill");

        assert!(read_skill_resource(&directory, Path::new("escape.md")).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn discovery_rejects_skill_directory_symlink_escape() {
        use std::os::unix::fs::symlink;

        let temporary = tempfile::tempdir().expect("temporary skills");
        let root = temporary.path().join("root");
        let outside = temporary.path().join("outside");
        std::fs::create_dir(&root).expect("create root");
        write_skill(&outside, "escaped", "escaped", "escaped");
        symlink(outside.join("escaped"), root.join("escaped")).expect("create escape");

        assert!(Skills::discover([root]).is_err());
    }

    fn write_skill(root: &std::path::Path, directory: &str, name: &str, description: &str) {
        let path = root.join(directory);
        std::fs::create_dir_all(&path).expect("create skill directory");
        std::fs::write(
            path.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: {description}\n---\n"),
        )
        .expect("write skill");
    }
}
