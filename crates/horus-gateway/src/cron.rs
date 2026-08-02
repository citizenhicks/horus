//! Gateway-owned scheduled task persistence, matching, and overlap locks.

use std::collections::BTreeSet;
use std::fs::{File, OpenOptions, TryLockError};
use std::io::{Read as _, Write as _};
#[cfg(unix)]
use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::str::FromStr as _;
use std::sync::Mutex;

use chrono::{Local, TimeZone as _, Utc};
use croner::Cron;
use horus::protocol::MAX_USER_INPUT_BYTES;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::wire::{CronRun, CronRunStatus, CronTask};
use crate::{Error, Result};

const STATE_VERSION: u32 = 1;
const STATE_FILE: &str = "cron.json";
const STATE_LOCK_FILE: &str = "cron-state.lock";
const MAX_STATE_BYTES: u64 = 1024 * 1024;
const MAX_RUNS: usize = 256;

/// Persistent cron state scoped to one configured gateway workspace.
pub(crate) struct CronStore {
    state_dir: PathBuf,
    workspace: Mutex<PathBuf>,
    path: PathBuf,
    state: Mutex<CronState>,
}

/// Result of reserving one task invocation.
pub(crate) enum BeginRun {
    Started(ActiveCronRun),
    Skipped,
}

/// A durable running invocation whose file lock is held until completion.
pub(crate) struct ActiveCronRun {
    run_id: String,
    _lock: File,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CronState {
    version: u32,
    tasks: Vec<CronTask>,
    runs: Vec<CronRun>,
}

impl Default for CronState {
    fn default() -> Self {
        Self {
            version: STATE_VERSION,
            tasks: Vec::new(),
            runs: Vec::new(),
        }
    }
}

impl CronStore {
    /// Opens or creates owner-only cron state for `workspace`.
    pub(crate) fn open(state_dir: &Path, workspace: &Path) -> Result<Self> {
        let state_dir = std::fs::canonicalize(state_dir)?;
        let workspace = std::fs::canonicalize(workspace)?;
        let path = state_dir.join(STATE_FILE);
        let mut state = match File::open(&path) {
            Ok(mut file) => {
                #[cfg(unix)]
                file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
                let mut contents = Vec::new();
                std::io::Read::by_ref(&mut file)
                    .take(MAX_STATE_BYTES + 1)
                    .read_to_end(&mut contents)?;
                if u64::try_from(contents.len()).unwrap_or(u64::MAX) > MAX_STATE_BYTES {
                    return Err(Error::Config("cron state is too large".into()));
                }
                serde_json::from_slice(&contents)?
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => CronState::default(),
            Err(error) => return Err(error.into()),
        };
        validate_state(&state, &workspace)?;
        let recovered = recover_interrupted_runs(&mut state);
        let store = Self {
            state_dir,
            workspace: Mutex::new(workspace),
            path,
            state: Mutex::new(state),
        };
        if recovered || !store.path.exists() {
            let state = store.lock_state()?;
            store.save(&state)?;
        }
        Ok(store)
    }

    /// Adds one task, returning an existing identical schedule without duplication.
    pub(crate) fn add(&self, task: &Path, schedule: &str) -> Result<(CronTask, bool)> {
        let task = self.canonical_task(task)?;
        let schedule = validate_schedule(schedule)?;
        self.update(|state| {
            if let Some(existing) = state.tasks.iter().find(|existing| existing.task == task) {
                if existing.schedule == schedule {
                    return Ok((existing.clone(), false));
                }
                return Err(Error::Config(format!(
                    "cron task {} already uses this task file; reschedule it",
                    existing.id
                )));
            }
            let task = CronTask {
                id: Uuid::new_v4().to_string(),
                task,
                schedule,
            };
            state.tasks.push(task.clone());
            Ok((task, true))
        })
    }

    /// Lists scheduled tasks in creation order.
    pub(crate) fn list(&self) -> Result<Vec<CronTask>> {
        Ok(self.lock_state()?.tasks.clone())
    }

    /// Changes task confinement only when no scheduled task can retain the old workspace.
    pub(crate) fn set_workspace(&self, workspace: &Path) -> Result<()> {
        let workspace = std::fs::canonicalize(workspace)?;
        if !workspace.is_dir() || workspace.parent().is_none() {
            return Err(Error::Config(
                "workspace must be an existing non-root directory".into(),
            ));
        }
        let state = self.lock_state()?;
        if !state.tasks.is_empty() {
            return Err(Error::Config(
                "delete all cron tasks before changing the workspace".into(),
            ));
        }
        *self.lock_workspace()? = workspace;
        Ok(())
    }

    /// Replaces one task's schedule, accepting an unambiguous ID prefix.
    pub(crate) fn reschedule(&self, id: &str, schedule: &str) -> Result<CronTask> {
        let schedule = validate_schedule(schedule)?;
        self.update(|state| {
            let index = resolve_task(&state.tasks, id)?;
            state.tasks[index].schedule = schedule;
            Ok(state.tasks[index].clone())
        })
    }

    /// Deletes one idle task, accepting an unambiguous ID prefix.
    pub(crate) fn delete(&self, id: &str) -> Result<CronTask> {
        let task = self.task(id)?;
        let Some(_lock) = self.try_task_lock(&task.id)? else {
            return Err(Error::Config(format!(
                "cron task {} is currently running",
                task.id
            )));
        };
        self.update(|state| {
            let index = resolve_task(&state.tasks, &task.id)?;
            Ok(state.tasks.remove(index))
        })
    }

    /// Resolves one task by full ID or unambiguous prefix.
    pub(crate) fn task(&self, id: &str) -> Result<CronTask> {
        let state = self.lock_state()?;
        Ok(state.tasks[resolve_task(&state.tasks, id)?].clone())
    }

    /// Reads a task after rechecking its path and input-size boundary.
    pub(crate) fn task_input(&self, id: &str) -> Result<(CronTask, String)> {
        let task = self.task(id)?;
        let workspace = self.workspace()?;
        let path = std::fs::canonicalize(&task.task)?;
        if !path.is_file() || !path.starts_with(&workspace) {
            return Err(Error::Config(
                "cron task must remain a file inside the gateway workspace".into(),
            ));
        }
        let mut file = File::open(&path)?;
        let opened = file.metadata()?;
        let verified = std::fs::canonicalize(&task.task)?;
        let current = std::fs::metadata(&verified)?;
        if verified != path || !same_file(&opened, &current) {
            return Err(Error::Config(
                "cron task changed while it was being opened".into(),
            ));
        }
        let limit = u64::try_from(MAX_USER_INPUT_BYTES).unwrap_or(u64::MAX);
        let mut bytes = Vec::new();
        std::io::Read::by_ref(&mut file)
            .take(limit + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() > MAX_USER_INPUT_BYTES {
            return Err(Error::Config(format!(
                "cron task exceeds the {MAX_USER_INPUT_BYTES}-byte input limit"
            )));
        }
        let input = String::from_utf8(bytes)
            .map_err(|_| Error::Config("cron task is not valid UTF-8".into()))?;
        if input.trim().is_empty() {
            return Err(Error::Config("cron task is empty".into()));
        }
        Ok((task, input))
    }

    /// Returns tasks matching one local-time Unix minute.
    pub(crate) fn due_at_minute(&self, unix_minute: i64) -> Result<Vec<CronTask>> {
        let seconds = unix_minute
            .checked_mul(60)
            .ok_or_else(|| Error::Config("cron timestamp overflow".into()))?;
        let time = Local
            .timestamp_opt(seconds, 0)
            .single()
            .ok_or_else(|| Error::Config("cron timestamp is outside the supported range".into()))?;
        self.lock_state()?
            .tasks
            .iter()
            .filter_map(|task| match Cron::from_str(&task.schedule) {
                Ok(schedule) => match schedule.is_time_matching(&time) {
                    Ok(true) => Some(Ok(task.clone())),
                    Ok(false) => None,
                    Err(error) => Some(Err(Error::Config(format!(
                        "invalid persisted cron schedule: {error}"
                    )))),
                },
                Err(error) => Some(Err(Error::Config(format!(
                    "invalid persisted cron schedule: {error}"
                )))),
            })
            .collect()
    }

    /// Returns the current Unix minute for scheduler de-duplication.
    pub(crate) fn current_unix_minute() -> i64 {
        Utc::now().timestamp().div_euclid(60)
    }

    /// Starts an overlap-locked invocation or records an overlap skip.
    pub(crate) fn begin_run(&self, id: &str) -> Result<BeginRun> {
        let task = self.task(id)?;
        let Some(lock) = self.try_task_lock(&task.id)? else {
            self.record_terminal_run(
                &task.id,
                CronRunStatus::Skipped,
                None,
                Some("the previous invocation is still running".into()),
            )?;
            return Ok(BeginRun::Skipped);
        };
        let run = CronRun {
            id: Uuid::new_v4().to_string(),
            task_id: task.id.clone(),
            started_at: Utc::now().timestamp(),
            finished_at: None,
            status: CronRunStatus::Running,
            session_id: None,
            message: None,
        };
        self.update(|state| {
            append_run(state, run.clone())?;
            Ok(())
        })?;
        Ok(BeginRun::Started(ActiveCronRun {
            run_id: run.id,
            _lock: lock,
        }))
    }

    /// Associates the newly-created agent session with a running invocation.
    pub(crate) fn attach_session(&self, run: &ActiveCronRun, session_id: &str) -> Result<()> {
        self.update(|state| {
            let stored = find_run_mut(state, &run.run_id)?;
            stored.session_id = Some(session_id.into());
            Ok(())
        })
    }

    /// Completes a running invocation and releases its overlap lock.
    pub(crate) fn finish_run(
        &self,
        run: ActiveCronRun,
        status: CronRunStatus,
        message: Option<String>,
    ) -> Result<CronRun> {
        if status == CronRunStatus::Running {
            return Err(Error::Config(
                "a completed cron run cannot remain running".into(),
            ));
        }
        self.update(|state| {
            let stored = find_run_mut(state, &run.run_id)?;
            stored.finished_at = Some(Utc::now().timestamp());
            stored.status = status;
            stored.message = message;
            Ok(stored.clone())
        })
    }

    /// Records a skipped invocation that could not enter the agent host.
    pub(crate) fn skip_run(&self, id: &str, message: impl Into<String>) -> Result<CronRun> {
        let task = self.task(id)?;
        self.record_terminal_run(&task.id, CronRunStatus::Skipped, None, Some(message.into()))
    }

    /// Returns newest-first run history, optionally scoped by task ID prefix.
    pub(crate) fn history(&self, id: Option<&str>) -> Result<Vec<CronRun>> {
        let state = self.lock_state()?;
        let task_id = id.map(|id| resolve_history_task(&state, id)).transpose()?;
        Ok(state
            .runs
            .iter()
            .rev()
            .filter(|run| task_id.as_ref().is_none_or(|id| &run.task_id == id))
            .cloned()
            .collect())
    }

    fn canonical_task(&self, task: &Path) -> Result<PathBuf> {
        let workspace = self.workspace()?;
        let candidate = if task.is_absolute() {
            task.to_path_buf()
        } else {
            workspace.join(task)
        };
        let task = std::fs::canonicalize(candidate)?;
        if !task.is_file() || !task.starts_with(&workspace) {
            return Err(Error::Config(
                "cron task must be a file inside the gateway workspace".into(),
            ));
        }
        Ok(task)
    }

    fn try_task_lock(&self, id: &str) -> Result<Option<File>> {
        let file = open_private_lock(self.state_dir.join(format!("cron-{id}.lock")))?;
        match file.try_lock() {
            Ok(()) => Ok(Some(file)),
            Err(TryLockError::WouldBlock) => Ok(None),
            Err(TryLockError::Error(error)) => Err(error.into()),
        }
    }

    fn record_terminal_run(
        &self,
        task_id: &str,
        status: CronRunStatus,
        session_id: Option<String>,
        message: Option<String>,
    ) -> Result<CronRun> {
        let now = Utc::now().timestamp();
        let run = CronRun {
            id: Uuid::new_v4().to_string(),
            task_id: task_id.into(),
            started_at: now,
            finished_at: Some(now),
            status,
            session_id,
            message,
        };
        self.update(|state| {
            append_run(state, run.clone())?;
            Ok(run)
        })
    }

    fn update<T>(&self, mutate: impl FnOnce(&mut CronState) -> Result<T>) -> Result<T> {
        let _file_lock = open_private_lock(self.state_dir.join(STATE_LOCK_FILE))?;
        _file_lock.lock()?;
        let mut state = self.lock_state()?;
        let mut next = state.clone();
        let result = mutate(&mut next)?;
        validate_state(&next, &self.workspace()?)?;
        self.save(&next)?;
        *state = next;
        Ok(result)
    }

    fn save(&self, state: &CronState) -> Result<()> {
        let contents = serde_json::to_vec_pretty(state)?;
        if u64::try_from(contents.len()).unwrap_or(u64::MAX) > MAX_STATE_BYTES {
            return Err(Error::Config("cron state is too large".into()));
        }
        let mut file = tempfile::NamedTempFile::new_in(&self.state_dir)?;
        #[cfg(unix)]
        file.as_file()
            .set_permissions(std::fs::Permissions::from_mode(0o600))?;
        file.write_all(&contents)?;
        file.as_file().sync_all()?;
        file.persist(&self.path).map_err(|error| error.error)?;
        Ok(())
    }

    fn lock_state(&self) -> Result<std::sync::MutexGuard<'_, CronState>> {
        self.state
            .lock()
            .map_err(|_| Error::Config("cron state lock is poisoned".into()))
    }

    fn lock_workspace(&self) -> Result<std::sync::MutexGuard<'_, PathBuf>> {
        self.workspace
            .lock()
            .map_err(|_| Error::Config("cron workspace lock is poisoned".into()))
    }

    fn workspace(&self) -> Result<PathBuf> {
        Ok(self.lock_workspace()?.clone())
    }
}

fn validate_schedule(schedule: &str) -> Result<String> {
    let fields = schedule.split_ascii_whitespace().collect::<Vec<_>>();
    if fields.len() != 5
        || fields.iter().any(|field| {
            field.is_empty()
                || !field.chars().all(|character| {
                    character.is_ascii_alphanumeric() || matches!(character, '*' | '/' | ',' | '-')
                })
        })
    {
        return Err(Error::Config(
            "schedule must be a five-field cron expression".into(),
        ));
    }
    let schedule = fields.join(" ");
    Cron::from_str(&schedule)
        .map_err(|error| Error::Config(format!("invalid cron schedule: {error}")))?;
    Ok(schedule)
}

fn validate_state(state: &CronState, workspace: &Path) -> Result<()> {
    if state.version != STATE_VERSION {
        return Err(Error::Config(format!(
            "unsupported cron state version {}",
            state.version
        )));
    }
    if state.runs.len() > MAX_RUNS {
        return Err(Error::Config("cron run history is too large".into()));
    }
    let mut ids = BTreeSet::new();
    let mut paths = BTreeSet::new();
    for task in &state.tasks {
        let parsed = Uuid::parse_str(&task.id)
            .map_err(|_| Error::Config("invalid persisted cron task ID".into()))?;
        if parsed.to_string() != task.id || !ids.insert(task.id.as_str()) {
            return Err(Error::Config("duplicate persisted cron task ID".into()));
        }
        if !task.task.is_absolute()
            || !task.task.starts_with(workspace)
            || !paths.insert(task.task.as_path())
        {
            return Err(Error::Config(
                "persisted cron task path is outside the gateway workspace".into(),
            ));
        }
        validate_schedule(&task.schedule)?;
    }
    let mut run_ids = BTreeSet::new();
    for run in &state.runs {
        if Uuid::parse_str(&run.id).is_err() || !run_ids.insert(run.id.as_str()) {
            return Err(Error::Config("invalid persisted cron run ID".into()));
        }
        if run.task_id.is_empty() {
            return Err(Error::Config("persisted cron run has no task ID".into()));
        }
    }
    Ok(())
}

fn recover_interrupted_runs(state: &mut CronState) -> bool {
    let now = Utc::now().timestamp();
    let mut changed = false;
    for run in &mut state.runs {
        if run.status == CronRunStatus::Running {
            run.status = CronRunStatus::Failed;
            run.finished_at = Some(now);
            run.message = Some("the gateway stopped before this run completed".into());
            changed = true;
        }
    }
    changed
}

fn resolve_task(tasks: &[CronTask], id: &str) -> Result<usize> {
    if id.is_empty() || id.chars().any(char::is_whitespace) {
        return Err(Error::Config("cron task ID cannot be empty".into()));
    }
    if let Some(index) = tasks.iter().position(|task| task.id == id) {
        return Ok(index);
    }
    let mut matches = tasks
        .iter()
        .enumerate()
        .filter(|(_, task)| task.id.starts_with(id));
    let (index, _) = matches
        .next()
        .ok_or_else(|| Error::Config(format!("unknown cron task `{id}`")))?;
    if matches.next().is_some() {
        return Err(Error::Config(format!(
            "cron task ID prefix `{id}` is ambiguous"
        )));
    }
    Ok(index)
}

fn resolve_history_task(state: &CronState, id: &str) -> Result<String> {
    let mut ids = state
        .tasks
        .iter()
        .map(|task| task.id.as_str())
        .chain(state.runs.iter().map(|run| run.task_id.as_str()))
        .filter(|task_id| task_id.starts_with(id))
        .collect::<BTreeSet<_>>();
    if ids.contains(id) {
        return Ok(id.into());
    }
    let resolved = ids
        .pop_first()
        .ok_or_else(|| Error::Config(format!("unknown cron task `{id}`")))?;
    if !ids.is_empty() {
        return Err(Error::Config(format!(
            "cron task ID prefix `{id}` is ambiguous"
        )));
    }
    Ok(resolved.into())
}

fn append_run(state: &mut CronState, run: CronRun) -> Result<()> {
    if state.runs.len() == MAX_RUNS {
        let index = state
            .runs
            .iter()
            .position(|run| run.status != CronRunStatus::Running)
            .ok_or_else(|| Error::Config("cron run history is full of active runs".into()))?;
        state.runs.remove(index);
    }
    state.runs.push(run);
    Ok(())
}

fn find_run_mut<'a>(state: &'a mut CronState, id: &str) -> Result<&'a mut CronRun> {
    state
        .runs
        .iter_mut()
        .find(|run| run.id == id)
        .ok_or_else(|| Error::Config(format!("unknown cron run `{id}`")))
}

fn open_private_lock(path: PathBuf) -> Result<File> {
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    options.mode(0o600);
    let file = options.open(path)?;
    #[cfg(unix)]
    file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    Ok(file)
}

#[cfg(unix)]
fn same_file(left: &std::fs::Metadata, right: &std::fs::Metadata) -> bool {
    left.dev() == right.dev() && left.ino() == right.ino()
}

#[cfg(not(unix))]
fn same_file(_left: &std::fs::Metadata, _right: &std::fs::Metadata) -> bool {
    true
}

#[cfg(test)]
mod tests {
    use chrono::{LocalResult, NaiveDate};

    use super::*;

    fn store() -> (tempfile::TempDir, PathBuf, CronStore) {
        let root = tempfile::tempdir().expect("temp dir");
        let workspace = root.path().join("workspace");
        let state = root.path().join("state");
        std::fs::create_dir(&workspace).expect("workspace");
        std::fs::create_dir(&state).expect("state");
        let store = CronStore::open(&state, &workspace).expect("cron store");
        (root, workspace, store)
    }

    #[test]
    fn task_paths_cannot_escape_the_gateway_workspace() {
        let (root, _workspace, store) = store();
        let outside = root.path().join("outside.md");
        std::fs::write(&outside, "outside").expect("outside task");

        let error = store
            .add(&outside, "0 9 * * *")
            .expect_err("outside task must fail");

        assert!(error.to_string().contains("inside the gateway workspace"));
    }

    #[test]
    fn workspace_change_requires_an_empty_task_catalog() {
        let (root, workspace, store) = store();
        let task = workspace.join("task.md");
        let replacement = root.path().join("replacement");
        std::fs::write(&task, "task").expect("task");
        std::fs::create_dir(&replacement).expect("replacement workspace");
        store.add(&task, "0 9 * * *").expect("add task");

        let error = store
            .set_workspace(&replacement)
            .expect_err("workspace change with tasks must fail");

        assert!(error.to_string().contains("delete all cron tasks"));
    }

    #[test]
    fn workspace_change_replaces_task_path_confinement() {
        let (root, workspace, store) = store();
        let old_task = workspace.join("old.md");
        let replacement = root.path().join("replacement");
        let new_task = replacement.join("new.md");
        std::fs::write(&old_task, "old").expect("old task");
        std::fs::create_dir(&replacement).expect("replacement workspace");
        std::fs::write(&new_task, "new").expect("new task");

        store.set_workspace(&replacement).expect("change workspace");

        assert!(store.add(&old_task, "0 9 * * *").is_err());
        assert!(store.add(&new_task, "0 9 * * *").is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn registered_task_cannot_be_replaced_with_an_outside_symlink() {
        let (root, workspace, store) = store();
        let task_path = workspace.join("task.md");
        let outside = root.path().join("outside.md");
        std::fs::write(&task_path, "inside").expect("inside task");
        std::fs::write(&outside, "outside").expect("outside task");
        let (task, _) = store.add(&task_path, "0 9 * * *").expect("add task");
        std::fs::remove_file(&task_path).expect("remove task");
        std::os::unix::fs::symlink(&outside, &task_path).expect("replace with symlink");

        let error = store
            .task_input(&task.id)
            .expect_err("replacement symlink must fail");

        assert!(error.to_string().contains("inside the gateway workspace"));
    }

    #[test]
    fn tasks_and_history_persist_with_owner_only_permissions() {
        let (root, workspace, store) = store();
        let task_path = workspace.join("task.md");
        std::fs::write(&task_path, "do work").expect("task");
        let (task, changed) = store.add(&task_path, "0 9 * * MON").expect("add task");
        assert!(changed);
        let run = match store.begin_run(&task.id).expect("begin run") {
            BeginRun::Started(run) => run,
            BeginRun::Skipped => panic!("first run must start"),
        };
        store
            .finish_run(run, CronRunStatus::Succeeded, None)
            .expect("finish run");
        drop(store);

        let reopened = CronStore::open(&root.path().join("state"), &workspace).expect("reopen");

        assert_eq!(reopened.list().expect("list"), vec![task]);
        assert_eq!(reopened.history(None).expect("history").len(), 1);
        #[cfg(unix)]
        assert_eq!(
            std::fs::metadata(root.path().join("state").join(STATE_FILE))
                .expect("metadata")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }

    #[test]
    fn due_matching_uses_standard_five_field_weekdays() {
        let (_root, workspace, store) = store();
        let task_path = workspace.join("task.md");
        std::fs::write(&task_path, "do work").expect("task");
        let (task, _) = store.add(&task_path, "30 8 * * 1").expect("add task");
        let local = match Local.from_local_datetime(
            &NaiveDate::from_ymd_opt(2026, 8, 3)
                .expect("date")
                .and_hms_opt(8, 30, 0)
                .expect("time"),
        ) {
            LocalResult::Single(value) => value,
            LocalResult::Ambiguous(first, _) => first,
            LocalResult::None => panic!("local test timestamp must exist"),
        };

        let due = store
            .due_at_minute(local.timestamp().div_euclid(60))
            .expect("due tasks");

        assert_eq!(due, vec![task]);
    }

    #[test]
    fn overlap_is_skipped_and_recorded() {
        let (_root, workspace, store) = store();
        let task_path = workspace.join("task.md");
        std::fs::write(&task_path, "do work").expect("task");
        let (task, _) = store.add(&task_path, "* * * * *").expect("add task");
        let active = match store.begin_run(&task.id).expect("begin run") {
            BeginRun::Started(run) => run,
            BeginRun::Skipped => panic!("first run must start"),
        };

        let skipped = match store.begin_run(&task.id).expect("overlap result") {
            BeginRun::Skipped => store
                .history(Some(&task.id))
                .expect("history")
                .into_iter()
                .next()
                .expect("skipped run"),
            BeginRun::Started(_) => panic!("overlap must not start"),
        };

        assert_eq!(skipped.status, CronRunStatus::Skipped);
        store
            .finish_run(active, CronRunStatus::Succeeded, None)
            .expect("finish run");
    }

    #[test]
    fn history_trimming_preserves_running_entries() {
        let running = CronRun {
            id: "running".into(),
            task_id: "task".into(),
            started_at: 0,
            finished_at: None,
            status: CronRunStatus::Running,
            session_id: None,
            message: None,
        };
        let mut state = CronState::default();
        state.runs.push(running.clone());
        for index in 1..MAX_RUNS {
            state.runs.push(CronRun {
                id: index.to_string(),
                task_id: "task".into(),
                started_at: 0,
                finished_at: Some(0),
                status: CronRunStatus::Succeeded,
                session_id: None,
                message: None,
            });
        }

        append_run(
            &mut state,
            CronRun {
                id: "new".into(),
                task_id: "task".into(),
                started_at: 1,
                finished_at: Some(1),
                status: CronRunStatus::Succeeded,
                session_id: None,
                message: None,
            },
        )
        .expect("append run");

        assert_eq!(state.runs.len(), MAX_RUNS);
        assert!(state.runs.contains(&running));
    }

    #[test]
    fn malformed_or_out_of_range_schedule_is_rejected() {
        assert!(validate_schedule("0 9 * *").is_err());
        assert!(validate_schedule("75 9 * * *").is_err());
        assert!(validate_schedule("0 9 * * MON").is_ok());
    }
}
