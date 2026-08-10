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

const STATE_VERSION: u32 = 2;
const STATE_FILE: &str = "cron.json";
const STATE_LOCK_FILE: &str = "cron-state.lock";
const TASKS_DIR: &str = "tasks";
const MAX_STATE_BYTES: u64 = 1024 * 1024;
const MAX_RUNS: usize = 256;

/// Gateway-wide persistent cron state partitioned by source session.
pub(crate) struct CronStore {
    state_dir: PathBuf,
    tasks_dir: PathBuf,
    setup_sessions: Mutex<BTreeSet<String>>,
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
    /// Opens or creates owner-only cron state.
    pub(crate) fn open(state_dir: &Path) -> Result<Self> {
        let state_dir = std::fs::canonicalize(state_dir)?;
        let tasks_dir = private_tasks_dir(&state_dir)?;
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
        validate_state(&state, &tasks_dir)?;
        let recovered = recover_interrupted_runs(&mut state);
        let store = Self {
            state_dir,
            tasks_dir,
            setup_sessions: Mutex::new(BTreeSet::new()),
            path,
            state: Mutex::new(state),
        };
        if recovered || !store.path.exists() {
            let state = store.lock_state()?;
            store.save(&state)?;
        }
        Ok(store)
    }

    fn register(
        &self,
        source_session_id: &str,
        task: PathBuf,
        schedule: String,
    ) -> Result<CronTask> {
        self.update(|state| {
            let task = CronTask {
                id: Uuid::new_v4().to_string(),
                session_id: source_session_id.into(),
                task,
                schedule,
            };
            state.tasks.push(task.clone());
            Ok(task)
        })
    }

    /// Starts one explicit conversational setup and returns its model input.
    pub(crate) fn begin_setup(
        &self,
        source_session_id: &str,
        task: Option<&str>,
    ) -> Result<String> {
        validate_session_id(source_session_id)?;
        let task = task.map(str::trim).filter(|task| !task.is_empty());
        let input = task.map_or_else(
            || {
                "Set up a recurring task. Ask me for the task and timing details, then use `schedule_task`."
                    .into()
            },
            |task| {
                format!(
                    "Set up this recurring task:\n\n{task}\n\nAsk only for missing timing details, then use `schedule_task`."
                )
            },
        );
        if input.len() > MAX_USER_INPUT_BYTES {
            return Err(Error::Config(format!(
                "cron setup exceeds the {MAX_USER_INPUT_BYTES}-byte input limit"
            )));
        }
        self.lock_setups()?.insert(source_session_id.into());
        Ok(input)
    }

    /// Ends an unfinished conversational setup.
    pub(crate) fn cancel_setup(&self, source_session_id: &str) {
        if let Ok(mut active) = self.setup_sessions.lock() {
            active.remove(source_session_id);
        }
    }

    /// Writes and registers one model-confirmed task in the private gateway task directory.
    pub(crate) fn add_managed(
        &self,
        source_session_id: &str,
        task: &str,
        schedule: &str,
    ) -> Result<CronTask> {
        validate_session_id(source_session_id)?;
        let mut active = self.lock_setups()?;
        if !active.contains(source_session_id) {
            return Err(Error::Config(
                "scheduled tasks require an active scheduling setup".into(),
            ));
        }
        let task = task.trim();
        if task.is_empty() {
            return Err(Error::Config("scheduled task cannot be empty".into()));
        }
        if task.len() > MAX_USER_INPUT_BYTES {
            return Err(Error::Config(format!(
                "scheduled task exceeds the {MAX_USER_INPUT_BYTES}-byte input limit"
            )));
        }
        let schedule = validate_schedule(schedule)?;
        let path = self
            .tasks_dir
            .join(format!("{}.md", Uuid::new_v4().as_hyphenated()));
        write_private_task(&self.tasks_dir, &path, task.as_bytes())?;
        match self.register(source_session_id, path.clone(), schedule) {
            Ok(task) => {
                active.remove(source_session_id);
                Ok(task)
            }
            Err(error) => match std::fs::remove_file(&path) {
                Ok(()) => Err(error),
                Err(rollback) => Err(Error::Config(format!(
                    "{error}; removing the unregistered task failed: {rollback}"
                ))),
            },
        }
    }

    #[cfg(test)]
    pub(crate) fn add_for_test(
        &self,
        source_session_id: &str,
        task: &str,
        schedule: &str,
    ) -> Result<CronTask> {
        self.begin_setup(source_session_id, Some(task))?;
        self.add_managed(source_session_id, task, schedule)
    }

    /// Lists one source session's scheduled tasks in creation order.
    pub(crate) fn list(&self, source_session_id: &str) -> Result<Vec<CronTask>> {
        Ok(self
            .lock_state()?
            .tasks
            .iter()
            .filter(|task| task.session_id == source_session_id)
            .cloned()
            .collect())
    }

    pub(crate) fn has_tasks(&self) -> Result<bool> {
        Ok(!self.lock_state()?.tasks.is_empty())
    }

    /// Replaces one task's schedule, accepting an unambiguous ID prefix.
    pub(crate) fn reschedule(
        &self,
        source_session_id: &str,
        id: &str,
        schedule: &str,
    ) -> Result<CronTask> {
        let schedule = validate_schedule(schedule)?;
        self.update(|state| {
            let index = resolve_task(&state.tasks, source_session_id, id)?;
            state.tasks[index].schedule = schedule;
            Ok(state.tasks[index].clone())
        })
    }

    /// Deletes one idle task, accepting an unambiguous ID prefix.
    pub(crate) fn delete(&self, source_session_id: &str, id: &str) -> Result<CronTask> {
        let task = self.task(source_session_id, id)?;
        let Some(_lock) = self.try_task_lock(&task.id)? else {
            return Err(Error::Config(format!(
                "cron task {} is currently running",
                task.id
            )));
        };
        let deleted = self.update(|state| {
            let index = resolve_task(&state.tasks, source_session_id, &task.id)?;
            Ok(state.tasks.remove(index))
        })?;
        match std::fs::remove_file(&deleted.task) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(Error::Config(format!(
                    "cron task {} was deleted, but its task file could not be removed: {error}",
                    deleted.id
                )));
            }
        }
        Ok(deleted)
    }

    /// Permanently removes one idle session's schedules and run history.
    pub(crate) fn delete_session(&self, source_session_id: &str) -> Result<()> {
        self.require_session_idle(source_session_id)?;
        let (tasks, locks) = self.lock_session_tasks(source_session_id)?;
        for task in &tasks {
            remove_if_present(&task.task)?;
        }
        self.update(|state| {
            state
                .tasks
                .retain(|task| task.session_id != source_session_id);
            state
                .runs
                .retain(|run| run.source_session_id != source_session_id);
            Ok(())
        })?;
        drop(locks);
        Ok(())
    }

    pub(crate) fn require_session_idle(&self, source_session_id: &str) -> Result<()> {
        validate_session_id(source_session_id)?;
        if self.lock_setups()?.contains(source_session_id) {
            return Err(Error::Config(
                "scheduled-task setup is currently active for this session".into(),
            ));
        }
        let _ = self.lock_session_tasks(source_session_id)?;
        Ok(())
    }

    /// Resolves one task by full ID or unambiguous prefix.
    pub(crate) fn task(&self, source_session_id: &str, id: &str) -> Result<CronTask> {
        let state = self.lock_state()?;
        Ok(state.tasks[resolve_task(&state.tasks, source_session_id, id)?].clone())
    }

    /// Reads a task after rechecking its path and input-size boundary.
    pub(crate) fn task_input(&self, id: &str) -> Result<(CronTask, String)> {
        let task = self.stored_task(id)?;
        let path = std::fs::canonicalize(&task.task)?;
        if !path.is_file() || path.parent() != Some(self.tasks_dir.as_path()) {
            return Err(Error::Config(
                "cron task must remain inside the private gateway task directory".into(),
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
        let task = self.stored_task(id)?;
        let Some(lock) = self.try_task_lock(&task.id)? else {
            self.record_terminal_run(
                &task,
                CronRunStatus::Skipped,
                Some("the previous invocation is still running".into()),
            )?;
            return Ok(BeginRun::Skipped);
        };
        let run = CronRun {
            id: Uuid::new_v4().to_string(),
            task_id: task.id.clone(),
            source_session_id: task.session_id,
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

    /// Associates the newly-created execution session with a running invocation.
    pub(crate) fn attach_execution_session(
        &self,
        run: &ActiveCronRun,
        execution_session_id: &str,
    ) -> Result<()> {
        validate_session_id(execution_session_id)?;
        self.update(|state| {
            let stored = find_run_mut(state, &run.run_id)?;
            stored.session_id = Some(execution_session_id.into());
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
        let task = self.stored_task(id)?;
        self.record_terminal_run(&task, CronRunStatus::Skipped, Some(message.into()))
    }

    /// Returns newest-first run history for one source session.
    pub(crate) fn history(
        &self,
        source_session_id: &str,
        id: Option<&str>,
    ) -> Result<Vec<CronRun>> {
        let state = self.lock_state()?;
        let task_id = id
            .map(|id| resolve_history_task(&state, source_session_id, id))
            .transpose()?;
        Ok(state
            .runs
            .iter()
            .rev()
            .filter(|run| {
                run.source_session_id == source_session_id
                    && task_id.as_ref().is_none_or(|id| &run.task_id == id)
            })
            .cloned()
            .collect())
    }

    fn stored_task(&self, id: &str) -> Result<CronTask> {
        self.lock_state()?
            .tasks
            .iter()
            .find(|task| task.id == id)
            .cloned()
            .ok_or_else(|| Error::Config(format!("unknown cron task `{id}`")))
    }

    fn try_task_lock(&self, id: &str) -> Result<Option<File>> {
        let file = open_private_lock(self.state_dir.join(format!("cron-{id}.lock")))?;
        match file.try_lock() {
            Ok(()) => Ok(Some(file)),
            Err(TryLockError::WouldBlock) => Ok(None),
            Err(TryLockError::Error(error)) => Err(error.into()),
        }
    }

    fn lock_session_tasks(&self, source_session_id: &str) -> Result<(Vec<CronTask>, Vec<File>)> {
        let tasks = self.list(source_session_id)?;
        let mut locks = Vec::with_capacity(tasks.len());
        for task in &tasks {
            let Some(lock) = self.try_task_lock(&task.id)? else {
                return Err(Error::Config(format!(
                    "cron task {} is currently running",
                    task.id
                )));
            };
            locks.push(lock);
        }
        Ok((tasks, locks))
    }

    fn record_terminal_run(
        &self,
        task: &CronTask,
        status: CronRunStatus,
        message: Option<String>,
    ) -> Result<CronRun> {
        let now = Utc::now().timestamp();
        let run = CronRun {
            id: Uuid::new_v4().to_string(),
            task_id: task.id.clone(),
            source_session_id: task.session_id.clone(),
            started_at: now,
            finished_at: Some(now),
            status,
            session_id: None,
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
        validate_state(&next, &self.tasks_dir)?;
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

    fn lock_setups(&self) -> Result<std::sync::MutexGuard<'_, BTreeSet<String>>> {
        self.setup_sessions
            .lock()
            .map_err(|_| Error::Config("cron setup lock is poisoned".into()))
    }
}

fn validate_session_id(session_id: &str) -> Result<()> {
    if session_id.trim().is_empty() {
        return Err(Error::Config("cron session ID cannot be empty".into()));
    }
    Ok(())
}

fn validate_task_id_prefix(id: &str) -> Result<()> {
    if id.is_empty() || id.chars().any(char::is_whitespace) {
        return Err(Error::Config("cron task ID cannot be empty".into()));
    }
    Ok(())
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

fn validate_state(state: &CronState, tasks_dir: &Path) -> Result<()> {
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
        validate_session_id(&task.session_id)?;
        if !task.task.is_absolute()
            || task.task.parent() != Some(tasks_dir)
            || !paths.insert(task.task.as_path())
        {
            return Err(Error::Config(
                "persisted cron task path is outside the private gateway task directory".into(),
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
        validate_session_id(&run.source_session_id)?;
        if let Some(session_id) = &run.session_id {
            validate_session_id(session_id)?;
        }
        if state
            .tasks
            .iter()
            .find(|task| task.id == run.task_id)
            .is_some_and(|task| task.session_id != run.source_session_id)
        {
            return Err(Error::Config(
                "persisted cron run source does not own its task".into(),
            ));
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

fn resolve_task(tasks: &[CronTask], source_session_id: &str, id: &str) -> Result<usize> {
    validate_task_id_prefix(id)?;
    if let Some(index) = tasks
        .iter()
        .position(|task| task.session_id == source_session_id && task.id == id)
    {
        return Ok(index);
    }
    let mut matches = tasks
        .iter()
        .enumerate()
        .filter(|(_, task)| task.session_id == source_session_id && task.id.starts_with(id));
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

fn resolve_history_task(state: &CronState, source_session_id: &str, id: &str) -> Result<String> {
    validate_task_id_prefix(id)?;
    let mut ids = state
        .tasks
        .iter()
        .filter(|task| task.session_id == source_session_id)
        .map(|task| task.id.as_str())
        .chain(
            state
                .runs
                .iter()
                .filter(|run| run.source_session_id == source_session_id)
                .map(|run| run.task_id.as_str()),
        )
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

fn private_tasks_dir(state_dir: &Path) -> Result<PathBuf> {
    let path = state_dir.join(TASKS_DIR);
    std::fs::create_dir_all(&path)?;
    let path = std::fs::canonicalize(path)?;
    if path.parent() != Some(state_dir) || !path.is_dir() {
        return Err(Error::Config(
            "gateway task directory must be a real directory inside gateway state".into(),
        ));
    }
    #[cfg(unix)]
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))?;
    Ok(path)
}

fn remove_if_present(path: &Path) -> Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn write_private_task(directory: &Path, path: &Path, contents: &[u8]) -> Result<()> {
    let mut file = tempfile::NamedTempFile::new_in(directory)?;
    #[cfg(unix)]
    file.as_file()
        .set_permissions(std::fs::Permissions::from_mode(0o600))?;
    file.write_all(contents)?;
    file.as_file().sync_all()?;
    file.persist_noclobber(path).map_err(|error| error.error)?;
    Ok(())
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

    fn store() -> (tempfile::TempDir, CronStore) {
        let root = tempfile::tempdir().expect("temp dir");
        let state = root.path().join("state");
        std::fs::create_dir(&state).expect("state");
        let store = CronStore::open(&state).expect("cron store");
        (root, store)
    }

    fn add_task(
        store: &CronStore,
        source_session_id: &str,
        task: &str,
        schedule: &str,
    ) -> CronTask {
        store
            .begin_setup(source_session_id, Some(task))
            .expect("begin setup");
        store
            .add_managed(source_session_id, task, schedule)
            .expect("add managed task")
    }

    #[cfg(unix)]
    #[test]
    fn managed_task_cannot_be_replaced_with_an_outside_symlink() {
        let (root, store) = store();
        let task = add_task(&store, "session-a", "inside", "0 9 * * *");
        let outside = root.path().join("outside.md");
        std::fs::write(&outside, "outside").expect("outside task");
        std::fs::remove_file(&task.task).expect("remove task");
        std::os::unix::fs::symlink(&outside, &task.task).expect("replace with symlink");

        let error = store
            .task_input(&task.id)
            .expect_err("replacement symlink must fail");

        assert!(error.to_string().contains("private gateway task directory"));
    }

    #[test]
    fn tasks_and_history_persist_with_source_and_owner_only_permissions() {
        let (root, store) = store();
        let task = add_task(&store, "session-a", "do work", "0 9 * * MON");
        assert_eq!(store.task_input(&task.id).expect("read task").1, "do work");
        let run = match store.begin_run(&task.id).expect("begin run") {
            BeginRun::Started(run) => run,
            BeginRun::Skipped => panic!("first run must start"),
        };
        store
            .attach_execution_session(&run, "execution-session")
            .expect("attach execution session");
        store
            .finish_run(run, CronRunStatus::Succeeded, None)
            .expect("finish run");
        drop(store);

        let reopened = CronStore::open(&root.path().join("state")).expect("reopen");
        let runs = reopened.history("session-a", None).expect("source history");

        assert_eq!(
            reopened.list("session-a").expect("source tasks"),
            vec![task.clone()]
        );
        assert!(reopened.list("session-b").expect("other tasks").is_empty());
        assert_eq!(runs.len(), 1);
        assert_eq!(task.session_id, "session-a");
        assert_eq!(runs[0].source_session_id, "session-a");
        assert_eq!(runs[0].session_id.as_deref(), Some("execution-session"));
        #[cfg(unix)]
        {
            assert_eq!(
                std::fs::metadata(root.path().join("state").join(STATE_FILE))
                    .expect("state metadata")
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
            assert_eq!(
                std::fs::metadata(root.path().join("state").join(TASKS_DIR))
                    .expect("task directory metadata")
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
            assert_eq!(
                std::fs::metadata(&task.task)
                    .expect("task metadata")
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn setup_authority_is_concurrent_and_consumed_per_session() {
        let (_root, store) = store();
        store
            .begin_setup("session-a", Some("task a"))
            .expect("begin a");
        store
            .begin_setup("session-b", Some("task b"))
            .expect("begin b");
        store.cancel_setup("unrelated-session");

        let task_a = store
            .add_managed("session-a", "task a", "0 9 * * *")
            .expect("schedule a");
        let task_b = store
            .add_managed("session-b", "task b", "0 10 * * *")
            .expect("schedule b");

        assert!(
            store
                .add_managed("session-a", "second a", "0 11 * * *")
                .is_err(),
            "successful creation must consume only its setup authority"
        );
        assert_eq!(store.list("session-a").expect("tasks a"), vec![task_a]);
        assert_eq!(store.list("session-b").expect("tasks b"), vec![task_b]);
    }

    #[test]
    fn task_operations_are_scoped_to_the_source_session() {
        let (_root, store) = store();
        let task_a = add_task(&store, "session-a", "task a", "0 9 * * *");
        let task_b = add_task(&store, "session-b", "task b", "0 10 * * *");
        let prefix_len = task_a
            .id
            .bytes()
            .zip(task_b.id.bytes())
            .position(|(left, right)| left != right)
            .expect("unique task IDs must differ")
            + 1;
        let foreign_prefix = &task_b.id[..prefix_len];

        assert!(store.task("session-a", foreign_prefix).is_err());
        assert!(
            store
                .reschedule("session-a", foreign_prefix, "0 11 * * *")
                .is_err()
        );
        assert!(store.delete("session-a", foreign_prefix).is_err());
        assert_eq!(
            store
                .reschedule("session-a", &task_a.id, "0 12 * * *")
                .expect("reschedule own task")
                .schedule,
            "0 12 * * *"
        );
        for task in [&task_a, &task_b] {
            let run = match store.begin_run(&task.id).expect("begin run") {
                BeginRun::Started(run) => run,
                BeginRun::Skipped => panic!("run must start"),
            };
            store
                .finish_run(run, CronRunStatus::Succeeded, None)
                .expect("finish run");
        }

        assert_eq!(
            store.history("session-a", None).expect("history a").len(),
            1
        );
        assert_eq!(
            store.history("session-b", None).expect("history b").len(),
            1
        );
        assert!(store.history("session-a", Some(foreign_prefix)).is_err());
        store
            .delete("session-a", &task_a.id)
            .expect("delete own task");
        assert_eq!(
            store
                .history("session-a", Some(&task_a.id))
                .expect("deleted task history")
                .len(),
            1
        );
    }

    #[test]
    fn delete_session_removes_only_its_schedules_files_and_history() {
        let (root, store) = store();
        let deleted = add_task(&store, "session-a", "task a", "0 9 * * *");
        let retained = add_task(&store, "session-b", "task b", "0 10 * * *");
        for task in [&deleted, &retained] {
            let run = match store.begin_run(&task.id).expect("begin run") {
                BeginRun::Started(run) => run,
                BeginRun::Skipped => panic!("run must start"),
            };
            store
                .finish_run(run, CronRunStatus::Succeeded, None)
                .expect("finish run");
        }

        store
            .delete_session("session-a")
            .expect("delete session cron data");

        assert!(store.list("session-a").expect("deleted tasks").is_empty());
        assert!(
            store
                .history("session-a", None)
                .expect("deleted history")
                .is_empty()
        );
        assert!(!deleted.task.exists());
        assert_eq!(store.list("session-b").expect("retained tasks"), [retained]);
        drop(store);
        let reopened = CronStore::open(&root.path().join("state")).expect("reopen");
        assert!(
            reopened
                .list("session-a")
                .expect("reopened tasks")
                .is_empty()
        );
        assert_eq!(
            reopened
                .history("session-b", None)
                .expect("retained history")
                .len(),
            1
        );
    }

    #[test]
    fn delete_session_rejects_a_running_schedule() {
        let (_root, store) = store();
        let task = add_task(&store, "session-a", "task a", "0 9 * * *");
        let run = match store.begin_run(&task.id).expect("begin run") {
            BeginRun::Started(run) => run,
            BeginRun::Skipped => panic!("run must start"),
        };

        assert!(store.delete_session("session-a").is_err());
        assert_eq!(store.list("session-a").expect("retained task"), [task]);

        store
            .finish_run(run, CronRunStatus::Succeeded, None)
            .expect("finish run");
    }

    #[test]
    fn delete_session_rejects_an_active_setup() {
        let (_root, store) = store();
        store
            .begin_setup("session-a", Some("task a"))
            .expect("begin setup");

        assert!(store.delete_session("session-a").is_err());

        store.cancel_setup("session-a");
        store
            .delete_session("session-a")
            .expect("delete idle session cron data");
    }

    #[test]
    fn missing_managed_file_does_not_block_schedule_deletion() {
        let (_root, store) = store();
        let task = add_task(&store, "session-a", "do work", "0 9 * * *");
        std::fs::remove_file(&task.task).expect("remove managed file");

        store
            .delete("session-a", &task.id)
            .expect("delete broken schedule");

        assert!(store.list("session-a").expect("list").is_empty());
    }

    #[test]
    fn cancelling_setup_is_scoped_to_its_session() {
        let (_root, store) = store();
        store.begin_setup("session-a", None).expect("begin a");
        store.begin_setup("session-b", None).expect("begin b");
        store.cancel_setup("session-a");

        assert!(
            store
                .add_managed("session-a", "task a", "0 9 * * *")
                .is_err()
        );
        assert!(
            store
                .add_managed("session-b", "task b", "0 10 * * *")
                .is_ok()
        );
    }

    #[test]
    fn ordinary_chat_cannot_create_a_scheduled_task() {
        let (_root, store) = store();
        store
            .begin_setup("setup-chat", None)
            .expect("begin setup in another chat");

        let error = store
            .add_managed("ordinary-chat", "Review open pull requests", "0 9 * * 1")
            .expect_err("setup authority is required");

        assert!(error.to_string().contains("active scheduling setup"));
        assert!(store.list("ordinary-chat").expect("list").is_empty());
    }

    #[test]
    fn due_matching_is_global_across_source_sessions() {
        let (_root, store) = store();
        let task_a = add_task(&store, "session-a", "task a", "30 8 * * 1");
        let task_b = add_task(&store, "session-b", "task b", "30 8 * * 1");
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

        assert_eq!(due, vec![task_a, task_b]);
    }

    #[test]
    fn overlap_is_skipped_and_recorded() {
        let (_root, store) = store();
        let task = add_task(&store, "session-a", "do work", "* * * * *");
        let active = match store.begin_run(&task.id).expect("begin run") {
            BeginRun::Started(run) => run,
            BeginRun::Skipped => panic!("first run must start"),
        };

        let skipped = match store.begin_run(&task.id).expect("overlap result") {
            BeginRun::Skipped => store
                .history("session-a", Some(&task.id))
                .expect("history")
                .into_iter()
                .next()
                .expect("skipped run"),
            BeginRun::Started(_) => panic!("overlap must not start"),
        };

        assert_eq!(skipped.status, CronRunStatus::Skipped);
        assert_eq!(skipped.source_session_id, "session-a");
        store
            .finish_run(active, CronRunStatus::Succeeded, None)
            .expect("finish run");
    }

    #[test]
    fn history_trimming_preserves_running_entries() {
        let running = CronRun {
            id: "running".into(),
            task_id: "task".into(),
            source_session_id: "source".into(),
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
                source_session_id: "source".into(),
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
                source_session_id: "source".into(),
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
    fn persisted_tasks_must_stay_in_the_private_task_directory() {
        let (root, store) = store();
        let mut state = CronState::default();
        state.tasks.push(CronTask {
            id: Uuid::new_v4().to_string(),
            session_id: "session-a".into(),
            task: root.path().join("outside.md"),
            schedule: "0 9 * * *".into(),
        });

        let error =
            validate_state(&state, &store.tasks_dir).expect_err("outside persisted task must fail");

        assert!(error.to_string().contains("private gateway task directory"));
    }

    #[test]
    fn previous_state_version_is_rejected_without_compatibility() {
        let root = tempfile::tempdir().expect("temp dir");
        let state_dir = root.path().join("state");
        std::fs::create_dir(&state_dir).expect("state");
        let state = CronState {
            version: STATE_VERSION - 1,
            tasks: Vec::new(),
            runs: Vec::new(),
        };
        std::fs::write(
            state_dir.join(STATE_FILE),
            serde_json::to_vec(&state).expect("encode old state"),
        )
        .expect("write old state");

        let error = match CronStore::open(&state_dir) {
            Ok(_) => panic!("old state must fail"),
            Err(error) => error,
        };

        assert!(error.to_string().contains("unsupported cron state version"));
    }

    #[test]
    fn malformed_or_out_of_range_schedule_is_rejected() {
        assert!(validate_schedule("0 9 * *").is_err());
        assert!(validate_schedule("75 9 * * *").is_err());
        assert!(validate_schedule("0 9 * * MON").is_ok());
    }
}
