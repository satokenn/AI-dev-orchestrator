//! Local persistence for tasks and their immutable attempt history.
//!
//! The ledger stores timestamps as Unix milliseconds. Timestamps are optional so
//! a queued attempt can be recorded before its provider starts.

use std::{fmt, path::Path, sync::Mutex};

use rusqlite::{Connection, OptionalExtension, params};

use crate::{
    AgentResult, Attempt, AttemptFailureReason, AttemptId, AttemptState, ProviderRef, Task, TaskId,
    TaskRole, TaskState, UsageCost, UsageMetric, ValidationCheckResult, ValidationResult,
};

/// A persisted attempt together with the task it belongs to and execution times.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AttemptRecord {
    task_id: TaskId,
    attempt: Attempt,
    started_at: Option<i64>,
    finished_at: Option<i64>,
}

impl AttemptRecord {
    #[must_use]
    pub fn new(
        task_id: TaskId,
        attempt: Attempt,
        started_at: Option<i64>,
        finished_at: Option<i64>,
    ) -> Self {
        Self {
            task_id,
            attempt,
            started_at,
            finished_at,
        }
    }

    #[must_use]
    pub const fn task_id(&self) -> &TaskId {
        &self.task_id
    }

    #[must_use]
    pub const fn attempt(&self) -> &Attempt {
        &self.attempt
    }

    #[must_use]
    pub const fn started_at(&self) -> Option<i64> {
        self.started_at
    }

    #[must_use]
    pub const fn finished_at(&self) -> Option<i64> {
        self.finished_at
    }
}

/// Errors returned by the local execution ledger.
#[derive(Debug)]
pub enum LedgerError {
    Sqlite(rusqlite::Error),
    InvalidStoredValue(String),
    UnsupportedSchemaVersion(u32),
}

impl fmt::Display for LedgerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Sqlite(error) => write!(formatter, "execution ledger database error: {error}"),
            Self::InvalidStoredValue(value) => {
                write!(formatter, "invalid execution ledger value: {value}")
            }
            Self::UnsupportedSchemaVersion(version) => {
                write!(
                    formatter,
                    "unsupported execution ledger schema version: {version}"
                )
            }
        }
    }
}

impl std::error::Error for LedgerError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Sqlite(error) => Some(error),
            Self::InvalidStoredValue(_) | Self::UnsupportedSchemaVersion(_) => None,
        }
    }
}

impl From<rusqlite::Error> for LedgerError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Sqlite(error)
    }
}

/// SQLite-backed local execution ledger.
pub struct SqliteExecutionLedger {
    connection: Mutex<Connection>,
}

type PublicationRow = (
    Option<String>,
    String,
    String,
    Option<String>,
    Option<String>,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
);
type PublicationTaskRow = (
    String,
    Option<String>,
    String,
    String,
    Option<String>,
    Option<String>,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
);

const LATEST_SCHEMA_VERSION: u32 = 6;

/// Repository boundary for local task and attempt history.
pub trait ExecutionLedger {
    fn save_task(&self, task: &Task) -> Result<(), LedgerError>;
    fn get_task(&self, task_id: &TaskId) -> Result<Option<Task>, LedgerError>;
    fn save_attempt(
        &self,
        task_id: &TaskId,
        attempt: &Attempt,
        started_at: Option<i64>,
        finished_at: Option<i64>,
    ) -> Result<(), LedgerError>;
    fn get_attempt(
        &self,
        task_id: &TaskId,
        attempt_id: &AttemptId,
    ) -> Result<Option<AttemptRecord>, LedgerError>;
    fn list_attempts(&self, task_id: &TaskId) -> Result<Vec<AttemptRecord>, LedgerError>;
}

impl SqliteExecutionLedger {
    /// Opens (and initializes) a ledger at `path`.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, LedgerError> {
        let connection = Connection::open(path)?;
        Self::from_connection(connection)
    }

    /// Opens an isolated in-memory ledger, useful for deterministic tests.
    pub fn open_in_memory() -> Result<Self, LedgerError> {
        Self::from_connection(Connection::open_in_memory()?)
    }

    /// Alias for [`Self::open_in_memory`].
    pub fn in_memory() -> Result<Self, LedgerError> {
        Self::open_in_memory()
    }

    fn from_connection(connection: Connection) -> Result<Self, LedgerError> {
        // SQLite only allows changing this setting outside a transaction.  Set it
        // before starting the schema transaction so it also protects migrations.
        connection.execute_batch("PRAGMA foreign_keys = ON;")?;
        let transaction = connection.unchecked_transaction()?;
        let version: u32 = transaction.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if version > LATEST_SCHEMA_VERSION {
            return Err(LedgerError::UnsupportedSchemaVersion(version));
        }
        let has_tasks: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'tasks')",
            [],
            |row| row.get::<_, i64>(0).map(|value| value != 0),
        )?;
        if !has_tasks {
            create_latest_schema(&transaction)?;
            set_schema_version(&transaction, LATEST_SCHEMA_VERSION)?;
        } else {
            transaction.execute_batch(
                "CREATE TABLE IF NOT EXISTS tasks (
                 id TEXT PRIMARY KEY NOT NULL,
                 description TEXT NOT NULL,
                 role TEXT NOT NULL,
                 state TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS attempts (
                 task_id TEXT NOT NULL,
                 id TEXT NOT NULL,
                 provider TEXT NOT NULL,
                 state TEXT NOT NULL,
                 started_at INTEGER,
                 finished_at INTEGER,
                 PRIMARY KEY (task_id, id),
                 FOREIGN KEY (task_id) REFERENCES tasks(id) ON DELETE CASCADE
             );
             CREATE TABLE IF NOT EXISTS agent_results (
                 task_id TEXT NOT NULL,
                 attempt_id TEXT NOT NULL,
                 summary TEXT NOT NULL,
                 reported_success INTEGER NOT NULL,
                 PRIMARY KEY (task_id, attempt_id),
                 FOREIGN KEY (task_id, attempt_id)
                     REFERENCES attempts(task_id, id) ON DELETE CASCADE
             );
             CREATE TABLE IF NOT EXISTS validation_results (
                 task_id TEXT NOT NULL,
                 attempt_id TEXT NOT NULL,
                 sequence INTEGER NOT NULL,
                 summary TEXT NOT NULL,
                 passed INTEGER NOT NULL,
                 PRIMARY KEY (task_id, attempt_id, sequence),
                 FOREIGN KEY (task_id, attempt_id)
                     REFERENCES attempts(task_id, id) ON DELETE CASCADE
             );
             CREATE TABLE IF NOT EXISTS validation_checks (
                 task_id TEXT NOT NULL,
                 attempt_id TEXT NOT NULL,
                 validation_sequence INTEGER NOT NULL,
                 sequence INTEGER NOT NULL,
                 name TEXT NOT NULL,
                 passed INTEGER NOT NULL,
                 exit_status INTEGER,
                 diagnostics TEXT NOT NULL,
                 PRIMARY KEY (task_id, attempt_id, validation_sequence, sequence),
                 FOREIGN KEY (task_id, attempt_id, validation_sequence)
                     REFERENCES validation_results(task_id, attempt_id, sequence)
                     ON DELETE CASCADE
             );
             CREATE TABLE IF NOT EXISTS usage_metrics (
                 task_id TEXT NOT NULL,
                 attempt_id TEXT NOT NULL,
                 sequence INTEGER NOT NULL,
                 name TEXT NOT NULL,
                 value TEXT NOT NULL,
                 unit TEXT NOT NULL,
                 PRIMARY KEY (task_id, attempt_id, sequence),
                 FOREIGN KEY (task_id, attempt_id)
                     REFERENCES attempts(task_id, id) ON DELETE CASCADE
             );
             CREATE TABLE IF NOT EXISTS publications (
                 idempotency_key TEXT PRIMARY KEY NOT NULL,
                 task_id TEXT,
                 repository TEXT NOT NULL,
                 branch TEXT NOT NULL,
                 commit_sha TEXT,
                 pull_request TEXT,
                 phase TEXT NOT NULL,
                 workspace TEXT, base TEXT, title TEXT, body TEXT
             );",
            )?;
            migrate_schema(&transaction, version)?;
        }
        transaction.commit()?;
        Ok(Self {
            connection: Mutex::new(connection),
        })
    }

    pub fn load_publication(
        &self,
        key: &str,
    ) -> Result<Option<crate::github_workflow::PublicationRecord>, LedgerError> {
        let connection = self.lock_connection()?;
        let row: Option<PublicationRow> = connection.query_row(
            "SELECT task_id, repository, branch, commit_sha, pull_request, phase, workspace, base, title, body FROM publications WHERE idempotency_key = ?1",
            params![key],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?, row.get(6)?, row.get(7)?, row.get(8)?, row.get(9)?)),
        ).optional()?;
        row.map(
            |(task_id, repository, branch, commit, pr, phase, workspace, base, title, body)| {
                crate::github_workflow::PublicationRecord::restore(
                    key.to_owned(),
                    task_id,
                    repository,
                    branch,
                    commit,
                    pr,
                    phase,
                    workspace,
                    base,
                    title,
                    body,
                )
            },
        )
        .transpose()
    }

    /// Looks up the publication associated with the conventional `issue-N` task id.
    /// Legacy records with a null task id are intentionally not inferred.
    pub fn load_publication_for_task(
        &self,
        task_id: &crate::TaskId,
    ) -> Result<Option<crate::github_workflow::PublicationRecord>, LedgerError> {
        let connection = self.lock_connection()?;
        let row: Option<PublicationTaskRow> = connection
            .query_row(
                "SELECT idempotency_key, task_id, repository, branch, commit_sha, pull_request, phase, workspace, base, title, body
             FROM publications WHERE task_id = ?1 ORDER BY rowid DESC LIMIT 1",
                params![task_id.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?, row.get(6)?, row.get(7)?, row.get(8)?, row.get(9)?, row.get(10)?)),
            )
            .optional()?;
        row.map(
            |(
                key,
                task_id,
                repository,
                branch,
                commit,
                pr,
                phase,
                workspace,
                base,
                title,
                body,
            )| {
                crate::github_workflow::PublicationRecord::restore(
                    key, task_id, repository, branch, commit, pr, phase, workspace, base, title,
                    body,
                )
            },
        )
        .transpose()
    }

    pub fn save_publication(
        &self,
        record: &crate::github_workflow::PublicationRecord,
    ) -> Result<(), LedgerError> {
        let connection = self.lock_connection()?;
        connection.execute(
            "INSERT INTO publications (idempotency_key, task_id, repository, branch, commit_sha, pull_request, phase, workspace, base, title, body)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
             ON CONFLICT(idempotency_key) DO UPDATE SET repository=excluded.repository,
             task_id=COALESCE(excluded.task_id, publications.task_id), branch=excluded.branch, commit_sha=excluded.commit_sha, pull_request=excluded.pull_request, phase=excluded.phase, workspace=COALESCE(excluded.workspace, publications.workspace), base=COALESCE(excluded.base, publications.base), title=COALESCE(excluded.title, publications.title), body=COALESCE(excluded.body, publications.body)",
            params![record.idempotency_key(), record.task_id(), record.repository(), record.branch(), record.commit_sha(), record.pull_request(), record.phase().as_str(), record.workspace().map(|p| p.to_string_lossy().into_owned()), record.base(), record.title(), record.body()],
        )?;
        Ok(())
    }

    /// Saves task metadata. Existing attempt rows are retained and loaded by
    /// [`Self::get_task`], so updating a task cannot erase its history.
    pub fn save_task(&self, task: &Task) -> Result<(), LedgerError> {
        let connection = self.lock_connection()?;
        connection.execute(
            "INSERT INTO tasks (id, description, role, state) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(id) DO UPDATE SET description = excluded.description,
                 role = excluded.role, state = excluded.state",
            params![
                task.id().as_str(),
                task.description(),
                task.role().as_str(),
                task_state_to_str(task.state()),
            ],
        )?;
        Ok(())
    }

    /// Retrieves a task and all of its attempts, ordered by insertion id.
    pub fn get_task(&self, task_id: &TaskId) -> Result<Option<Task>, LedgerError> {
        let connection = self.lock_connection()?;
        let task = connection
            .query_row(
                "SELECT description, role, state FROM tasks WHERE id = ?1",
                params![task_id.as_str()],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .optional()?;
        let Some((description, role, state)) = task else {
            return Ok(None);
        };
        let attempts = self.load_attempts(&connection, task_id)?;
        Ok(Some(Task::restore(
            task_id.clone(),
            description,
            TaskRole::new(role),
            task_state_from_str(&state)?,
            attempts.into_iter().map(|record| record.attempt).collect(),
        )))
    }

    /// Inserts or updates one attempt without affecting any other attempt.
    pub fn save_attempt(
        &self,
        task_id: &TaskId,
        attempt: &Attempt,
        started_at: Option<i64>,
        finished_at: Option<i64>,
    ) -> Result<(), LedgerError> {
        let record = AttemptRecord::new(task_id.clone(), attempt.clone(), started_at, finished_at);
        self.save_attempt_record(&record)
    }

    /// Inserts or updates one attempt record.
    pub fn save_attempt_record(&self, record: &AttemptRecord) -> Result<(), LedgerError> {
        let connection = self.lock_connection()?;
        let transaction = connection.unchecked_transaction()?;
        transaction.execute(
            "INSERT INTO attempts (task_id, id, provider, state, started_at, finished_at, failure_reason)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(task_id, id) DO UPDATE SET provider = excluded.provider,
                 state = excluded.state, started_at = excluded.started_at,
                 finished_at = excluded.finished_at, failure_reason = excluded.failure_reason",
            params![
                record.task_id().as_str(),
                record.attempt().id().as_str(),
                record.attempt().provider().as_str(),
                attempt_state_to_str(record.attempt().state()),
                record.started_at(),
                record.finished_at(),
                record.attempt().failure_reason().map(failure_reason_to_str),
            ],
        )?;
        transaction.execute(
            "DELETE FROM agent_results WHERE task_id = ?1 AND attempt_id = ?2",
            params![record.task_id().as_str(), record.attempt().id().as_str()],
        )?;
        if let Some(result) = record.attempt().agent_result() {
            transaction.execute(
                "INSERT INTO agent_results
                 (task_id, attempt_id, summary, reported_success) VALUES (?1, ?2, ?3, ?4)",
                params![
                    record.task_id().as_str(),
                    record.attempt().id().as_str(),
                    result.summary(),
                    bool_to_int(result.reported_success()),
                ],
            )?;
        }
        transaction.execute(
            "DELETE FROM validation_results WHERE task_id = ?1 AND attempt_id = ?2",
            params![record.task_id().as_str(), record.attempt().id().as_str()],
        )?;
        for (sequence, result) in record.attempt().validation_results().iter().enumerate() {
            let sequence = i64::try_from(sequence).map_err(|_| {
                LedgerError::InvalidStoredValue("validation sequence overflow".into())
            })?;
            transaction.execute(
                "INSERT INTO validation_results
                 (task_id, attempt_id, sequence, summary, passed) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    record.task_id().as_str(),
                    record.attempt().id().as_str(),
                    sequence,
                    result.summary(),
                    bool_to_int(result.passed()),
                ],
            )?;
            for (check_sequence, check) in result.checks().iter().enumerate() {
                let check_sequence = i64::try_from(check_sequence).map_err(|_| {
                    LedgerError::InvalidStoredValue("validation check sequence overflow".into())
                })?;
                transaction.execute(
                    "INSERT INTO validation_checks
                     (task_id, attempt_id, validation_sequence, sequence, name, passed,
                      exit_status, diagnostics)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                    params![
                        record.task_id().as_str(),
                        record.attempt().id().as_str(),
                        sequence,
                        check_sequence,
                        check.name(),
                        bool_to_int(check.passed()),
                        check.exit_status(),
                        check.diagnostics(),
                    ],
                )?;
            }
        }
        transaction.execute(
            "DELETE FROM usage_metrics WHERE task_id = ?1 AND attempt_id = ?2",
            params![record.task_id().as_str(), record.attempt().id().as_str()],
        )?;
        if let Some(usage) = record.attempt().usage_cost() {
            for (sequence, metric) in usage.metrics().iter().enumerate() {
                transaction.execute(
                    "INSERT INTO usage_metrics
                     (task_id, attempt_id, sequence, name, value, unit)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![
                        record.task_id().as_str(),
                        record.attempt().id().as_str(),
                        i64::try_from(sequence).map_err(|_| {
                            LedgerError::InvalidStoredValue("usage sequence overflow".into())
                        })?,
                        metric.name(),
                        metric.value(),
                        metric.unit(),
                    ],
                )?;
            }
        }
        transaction.commit()?;
        Ok(())
    }

    /// Retrieves one attempt for a task.
    pub fn get_attempt(
        &self,
        task_id: &TaskId,
        attempt_id: &AttemptId,
    ) -> Result<Option<AttemptRecord>, LedgerError> {
        let connection = self.lock_connection()?;
        let record = connection
            .query_row(
                "SELECT provider, state, started_at, finished_at, failure_reason
                 FROM attempts WHERE task_id = ?1 AND id = ?2",
                params![task_id.as_str(), attempt_id.as_str()],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<i64>>(2)?,
                        row.get::<_, Option<i64>>(3)?,
                        row.get::<_, Option<String>>(4)?,
                    ))
                },
            )
            .optional()?;
        let Some((provider, state, started_at, finished_at, reason)) = record else {
            return Ok(None);
        };
        let attempt = self.load_attempt_details(
            &connection,
            task_id,
            attempt_id,
            ProviderRef::new(provider),
            attempt_state_from_str(&state)?,
            reason.map(|r| failure_reason_from_str(&r)).transpose()?,
        )?;
        Ok(Some(AttemptRecord::new(
            task_id.clone(),
            attempt,
            started_at,
            finished_at,
        )))
    }

    /// Retrieves every attempt for a task in insertion order.
    pub fn list_attempts(&self, task_id: &TaskId) -> Result<Vec<AttemptRecord>, LedgerError> {
        let connection = self.lock_connection()?;
        self.load_attempts(&connection, task_id)
    }

    fn load_attempts(
        &self,
        connection: &Connection,
        task_id: &TaskId,
    ) -> Result<Vec<AttemptRecord>, LedgerError> {
        let mut statement = connection.prepare(
            "SELECT id, provider, state, started_at, finished_at, failure_reason
             FROM attempts WHERE task_id = ?1 ORDER BY rowid",
        )?;
        let rows = statement.query_map(params![task_id.as_str()], |row| {
            Ok((
                AttemptId::new(row.get::<_, String>(0)?),
                ProviderRef::new(row.get::<_, String>(1)?),
                row.get::<_, String>(2)?,
                row.get::<_, Option<i64>>(3)?,
                row.get::<_, Option<i64>>(4)?,
                row.get::<_, Option<String>>(5)?,
            ))
        })?;
        let mut attempts = Vec::new();
        for row in rows {
            let (attempt_id, provider, state, started_at, finished_at, reason) = row?;
            let attempt = self.load_attempt_details(
                connection,
                task_id,
                &attempt_id,
                provider,
                attempt_state_from_str(&state)?,
                reason.map(|r| failure_reason_from_str(&r)).transpose()?,
            )?;
            attempts.push(AttemptRecord::new(
                task_id.clone(),
                attempt,
                started_at,
                finished_at,
            ));
        }
        Ok(attempts)
    }

    fn load_attempt_details(
        &self,
        connection: &Connection,
        task_id: &TaskId,
        attempt_id: &AttemptId,
        provider: ProviderRef,
        state: AttemptState,
        failure_reason: Option<AttemptFailureReason>,
    ) -> Result<Attempt, LedgerError> {
        let agent_result = connection
            .query_row(
                "SELECT summary, reported_success FROM agent_results
                 WHERE task_id = ?1 AND attempt_id = ?2",
                params![task_id.as_str(), attempt_id.as_str()],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
            )
            .optional()?;
        let agent_result = agent_result
            .map(|(summary, reported_success)| {
                int_to_bool(reported_success)
                    .map(|reported_success| AgentResult::new(summary, reported_success))
            })
            .transpose()?;
        let aggregate_rows = {
            let mut statement = connection.prepare(
                "SELECT sequence, summary, passed FROM validation_results
                 WHERE task_id = ?1 AND attempt_id = ?2 ORDER BY sequence",
            )?;
            let rows =
                statement.query_map(params![task_id.as_str(), attempt_id.as_str()], |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, i64>(2)?,
                    ))
                })?;
            rows.collect::<Result<Vec<_>, _>>()?
        };
        let mut validations = Vec::with_capacity(aggregate_rows.len());
        for (sequence, summary, passed) in aggregate_rows {
            let mut statement = connection.prepare(
                "SELECT name, passed, exit_status, diagnostics
                 FROM validation_checks
                 WHERE task_id = ?1 AND attempt_id = ?2 AND validation_sequence = ?3
                 ORDER BY sequence",
            )?;
            let rows = statement.query_map(
                params![task_id.as_str(), attempt_id.as_str(), sequence],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, Option<i32>>(2)?,
                        row.get::<_, String>(3)?,
                    ))
                },
            )?;
            let mut checks = Vec::new();
            for row in rows {
                let (name, check_passed, exit_status, diagnostics) = row?;
                checks.push(ValidationCheckResult::new(
                    name,
                    int_to_bool(check_passed)?,
                    exit_status,
                    diagnostics,
                ));
            }
            validations.push(ValidationResult::restore(
                summary,
                int_to_bool(passed)?,
                checks,
            ));
        }
        let mut metrics = Vec::new();
        let mut statement = connection.prepare(
            "SELECT name, value, unit FROM usage_metrics
             WHERE task_id = ?1 AND attempt_id = ?2 ORDER BY sequence",
        )?;
        let rows = statement.query_map(params![task_id.as_str(), attempt_id.as_str()], |row| {
            Ok(UsageMetric::new(
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;
        for row in rows {
            metrics.push(row?);
        }
        let usage_cost = (!metrics.is_empty()).then(|| UsageCost::new(metrics));
        Ok(Attempt::restore(
            attempt_id.clone(),
            provider,
            state,
            agent_result,
            validations,
            usage_cost,
            failure_reason,
        ))
    }

    fn lock_connection(&self) -> Result<std::sync::MutexGuard<'_, Connection>, LedgerError> {
        self.connection.lock().map_err(|_| {
            LedgerError::InvalidStoredValue("execution ledger mutex was poisoned".into())
        })
    }
}

fn set_schema_version(connection: &Connection, version: u32) -> Result<(), rusqlite::Error> {
    connection.execute_batch(&format!("PRAGMA user_version = {version};"))
}

fn create_latest_schema(connection: &Connection) -> Result<(), rusqlite::Error> {
    connection.execute_batch(
        "CREATE TABLE tasks (id TEXT PRIMARY KEY NOT NULL, description TEXT NOT NULL, role TEXT NOT NULL, state TEXT NOT NULL);
         CREATE TABLE attempts (task_id TEXT NOT NULL, id TEXT NOT NULL, provider TEXT NOT NULL, state TEXT NOT NULL,
             started_at INTEGER, finished_at INTEGER, failure_reason TEXT, PRIMARY KEY (task_id, id),
             FOREIGN KEY (task_id) REFERENCES tasks(id) ON DELETE CASCADE);
         CREATE TABLE agent_results (task_id TEXT NOT NULL, attempt_id TEXT NOT NULL, summary TEXT NOT NULL,
             reported_success INTEGER NOT NULL, PRIMARY KEY (task_id, attempt_id),
             FOREIGN KEY (task_id, attempt_id) REFERENCES attempts(task_id, id) ON DELETE CASCADE);
         CREATE TABLE validation_results (task_id TEXT NOT NULL, attempt_id TEXT NOT NULL, sequence INTEGER NOT NULL,
             summary TEXT NOT NULL, passed INTEGER NOT NULL, PRIMARY KEY (task_id, attempt_id, sequence),
             FOREIGN KEY (task_id, attempt_id) REFERENCES attempts(task_id, id) ON DELETE CASCADE);
         CREATE TABLE validation_checks (task_id TEXT NOT NULL, attempt_id TEXT NOT NULL, validation_sequence INTEGER NOT NULL,
             sequence INTEGER NOT NULL, name TEXT NOT NULL, passed INTEGER NOT NULL, exit_status INTEGER, diagnostics TEXT NOT NULL,
             PRIMARY KEY (task_id, attempt_id, validation_sequence, sequence),
             FOREIGN KEY (task_id, attempt_id, validation_sequence)
                 REFERENCES validation_results(task_id, attempt_id, sequence) ON DELETE CASCADE);
         CREATE TABLE usage_metrics (task_id TEXT NOT NULL, attempt_id TEXT NOT NULL, sequence INTEGER NOT NULL,
             name TEXT NOT NULL, value TEXT NOT NULL, unit TEXT NOT NULL, PRIMARY KEY (task_id, attempt_id, sequence),
             FOREIGN KEY (task_id, attempt_id) REFERENCES attempts(task_id, id) ON DELETE CASCADE);
         CREATE TABLE publications (idempotency_key TEXT PRIMARY KEY NOT NULL, task_id TEXT, repository TEXT NOT NULL,
             branch TEXT NOT NULL, commit_sha TEXT, pull_request TEXT, phase TEXT NOT NULL,
             workspace TEXT, base TEXT, title TEXT, body TEXT);",
    )
}

fn column_exists(
    connection: &Connection,
    table: &str,
    column: &str,
) -> Result<bool, rusqlite::Error> {
    let mut statement = connection.prepare(&format!("PRAGMA table_info({table})"))?;
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        if row.get::<_, String>(1)? == column {
            return Ok(true);
        }
    }
    Ok(false)
}

fn add_column_if_missing(
    connection: &Connection,
    table: &str,
    column: &str,
    definition: &str,
) -> Result<(), rusqlite::Error> {
    if !column_exists(connection, table, column)? {
        connection.execute(
            &format!("ALTER TABLE {table} ADD COLUMN {column} {definition}"),
            [],
        )?;
    }
    Ok(())
}

fn migrate_schema(connection: &Connection, version: u32) -> Result<(), LedgerError> {
    // Each step is idempotent so version-zero databases from every historical
    // generation can be upgraded while retaining all existing row values.
    for target in (version + 1)..=LATEST_SCHEMA_VERSION {
        match target {
            1 => add_column_if_missing(connection, "attempts", "failure_reason", "TEXT")?,
            2 => add_column_if_missing(connection, "publications", "task_id", "TEXT")?,
            3 => add_column_if_missing(connection, "publications", "workspace", "TEXT")?,
            4 => add_column_if_missing(connection, "publications", "base", "TEXT")?,
            5 => add_column_if_missing(connection, "publications", "title", "TEXT")?,
            6 => add_column_if_missing(connection, "publications", "body", "TEXT")?,
            _ => unreachable!(),
        }
        set_schema_version(connection, target)?;
    }
    Ok(())
}

impl ExecutionLedger for SqliteExecutionLedger {
    fn save_task(&self, task: &Task) -> Result<(), LedgerError> {
        Self::save_task(self, task)
    }

    fn get_task(&self, task_id: &TaskId) -> Result<Option<Task>, LedgerError> {
        Self::get_task(self, task_id)
    }

    fn save_attempt(
        &self,
        task_id: &TaskId,
        attempt: &Attempt,
        started_at: Option<i64>,
        finished_at: Option<i64>,
    ) -> Result<(), LedgerError> {
        Self::save_attempt(self, task_id, attempt, started_at, finished_at)
    }

    fn get_attempt(
        &self,
        task_id: &TaskId,
        attempt_id: &AttemptId,
    ) -> Result<Option<AttemptRecord>, LedgerError> {
        Self::get_attempt(self, task_id, attempt_id)
    }

    fn list_attempts(&self, task_id: &TaskId) -> Result<Vec<AttemptRecord>, LedgerError> {
        Self::list_attempts(self, task_id)
    }
}

fn bool_to_int(value: bool) -> i64 {
    i64::from(value)
}

fn int_to_bool(value: i64) -> Result<bool, LedgerError> {
    match value {
        0 => Ok(false),
        1 => Ok(true),
        other => Err(LedgerError::InvalidStoredValue(format!(
            "boolean value {other}"
        ))),
    }
}

fn task_state_to_str(state: TaskState) -> &'static str {
    match state {
        TaskState::Pending => "pending",
        TaskState::Active => "active",
        TaskState::Completed => "completed",
        TaskState::Failed => "failed",
        TaskState::Cancelled => "cancelled",
    }
}

fn task_state_from_str(value: &str) -> Result<TaskState, LedgerError> {
    match value {
        "pending" => Ok(TaskState::Pending),
        "active" => Ok(TaskState::Active),
        "completed" => Ok(TaskState::Completed),
        "failed" => Ok(TaskState::Failed),
        "cancelled" => Ok(TaskState::Cancelled),
        other => Err(LedgerError::InvalidStoredValue(format!(
            "task state {other}"
        ))),
    }
}

fn attempt_state_to_str(state: AttemptState) -> &'static str {
    match state {
        AttemptState::Queued => "queued",
        AttemptState::Running => "running",
        AttemptState::Validating => "validating",
        AttemptState::Succeeded => "succeeded",
        AttemptState::Failed => "failed",
        AttemptState::Cancelled => "cancelled",
    }
}

fn attempt_state_from_str(value: &str) -> Result<AttemptState, LedgerError> {
    match value {
        "queued" => Ok(AttemptState::Queued),
        "running" => Ok(AttemptState::Running),
        "validating" => Ok(AttemptState::Validating),
        "succeeded" => Ok(AttemptState::Succeeded),
        "failed" => Ok(AttemptState::Failed),
        "cancelled" => Ok(AttemptState::Cancelled),
        other => Err(LedgerError::InvalidStoredValue(format!(
            "attempt state {other}"
        ))),
    }
}

fn failure_reason_to_str(reason: &AttemptFailureReason) -> &'static str {
    match reason {
        AttemptFailureReason::Provider => "provider",
        AttemptFailureReason::Timeout => "timeout",
        AttemptFailureReason::Cancelled => "cancelled",
        AttemptFailureReason::Workspace => "workspace",
        AttemptFailureReason::Validation => "validation",
    }
}
fn failure_reason_from_str(value: &str) -> Result<AttemptFailureReason, LedgerError> {
    match value {
        "provider" => Ok(AttemptFailureReason::Provider),
        "timeout" => Ok(AttemptFailureReason::Timeout),
        "cancelled" => Ok(AttemptFailureReason::Cancelled),
        "workspace" => Ok(AttemptFailureReason::Workspace),
        "validation" => Ok(AttemptFailureReason::Validation),
        _ => Err(LedgerError::InvalidStoredValue(value.to_owned())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task() -> Task {
        Task::new(
            TaskId::new("task-1"),
            "implement ledger",
            TaskRole::new("developer"),
        )
    }

    fn attempt(id: &str, provider: &str) -> Attempt {
        Attempt::new(AttemptId::new(id), ProviderRef::new(provider))
    }

    #[test]
    fn schema_is_initialized_and_task_round_trips() {
        let ledger = SqliteExecutionLedger::open_in_memory().unwrap();
        let task = task();
        ledger.save_task(&task).unwrap();
        assert_eq!(ledger.get_task(task.id()).unwrap(), Some(task));
    }

    #[test]
    fn foreign_key_enforcement_is_enabled_before_schema_transaction() {
        let ledger = SqliteExecutionLedger::open_in_memory().unwrap();
        let connection = ledger.connection.lock().unwrap();
        let enabled: i64 = connection
            .query_row("PRAGMA foreign_keys", [], |row| row.get(0))
            .unwrap();
        assert_eq!(enabled, 1);
        let error = connection
            .execute(
                "INSERT INTO attempts (task_id, id, provider, state) VALUES ('missing', 'a', 'codex', 'queued')",
                [],
            )
            .unwrap_err();
        assert!(matches!(error, rusqlite::Error::SqliteFailure(_, _)));
    }

    #[test]
    fn invalid_publication_phase_is_rejected_when_restored() {
        let ledger = SqliteExecutionLedger::open_in_memory().unwrap();
        ledger
            .connection
            .lock()
            .unwrap()
            .execute(
                "INSERT INTO publications (idempotency_key, repository, branch, phase) VALUES ('bad', 'o/r', 'main', 'future')",
                [],
            )
            .unwrap();
        assert!(matches!(
            ledger.load_publication("bad"),
            Err(LedgerError::InvalidStoredValue(_))
        ));
    }

    #[test]
    fn published_publication_without_pull_request_is_rejected_when_restored() {
        let ledger = SqliteExecutionLedger::open_in_memory().unwrap();
        ledger
            .connection
            .lock()
            .unwrap()
            .execute(
                "INSERT INTO publications (idempotency_key, repository, branch, phase) VALUES ('missing-pr', 'o/r', 'main', 'published')",
                [],
            )
            .unwrap();
        assert!(matches!(
            ledger.load_publication("missing-pr"),
            Err(LedgerError::InvalidStoredValue(_))
        ));
    }

    #[test]
    fn attempt_round_trips_results_state_and_timestamps() {
        let ledger = SqliteExecutionLedger::open_in_memory().unwrap();
        let task = task();
        ledger.save_task(&task).unwrap();
        let mut attempt = attempt("attempt-1", "codex");
        attempt.start().unwrap();
        attempt
            .record_agent_result(AgentResult::new("done", true))
            .unwrap();
        attempt.finish().unwrap();
        attempt
            .apply_validation(ValidationResult::new("tests passed", true))
            .unwrap();
        attempt.set_usage_cost(UsageCost::new([UsageMetric::new("tokens", "42", "count")]));
        ledger
            .save_attempt(task.id(), &attempt, Some(1_000), Some(2_000))
            .unwrap();
        let record = ledger
            .get_attempt(task.id(), attempt.id())
            .unwrap()
            .unwrap();
        assert_eq!(record.attempt(), &attempt);
        assert_eq!(record.started_at(), Some(1_000));
        assert_eq!(record.finished_at(), Some(2_000));
    }

    #[test]
    fn validation_round_trips_aggregate_and_all_check_details() {
        let ledger = SqliteExecutionLedger::open_in_memory().unwrap();
        let task = task();
        ledger.save_task(&task).unwrap();
        let mut attempt = attempt("attempt-1", "codex");
        attempt.start().unwrap();
        attempt.finish().unwrap();
        let validation = ValidationResult::from_checks(
            "one check failed",
            [
                ValidationCheckResult::new("cargo test", true, Some(0), ""),
                ValidationCheckResult::new(
                    "cargo clippy",
                    false,
                    None,
                    "warning: diagnostic details",
                ),
            ],
        );
        attempt.apply_validation(validation.clone()).unwrap();
        ledger
            .save_attempt(task.id(), &attempt, Some(10), Some(20))
            .unwrap();

        let loaded = ledger
            .get_attempt(task.id(), attempt.id())
            .unwrap()
            .unwrap();
        assert_eq!(loaded.attempt().validation_results(), &[validation]);
        assert_eq!(loaded.attempt().state(), AttemptState::Failed);
    }

    #[test]
    fn retry_adds_history_without_overwriting_previous_attempt() {
        let ledger = SqliteExecutionLedger::open_in_memory().unwrap();
        let task = task();
        ledger.save_task(&task).unwrap();
        let first = attempt("attempt-1", "codex");
        let second = attempt("attempt-2", "antigravity");
        ledger
            .save_attempt(task.id(), &first, Some(10), Some(20))
            .unwrap();
        ledger
            .save_attempt(task.id(), &second, Some(30), None)
            .unwrap();
        let attempts = ledger.list_attempts(task.id()).unwrap();
        assert_eq!(attempts.len(), 2);
        assert_eq!(attempts[0].attempt().provider().as_str(), "codex");
        assert_eq!(attempts[1].attempt().provider().as_str(), "antigravity");
        assert_eq!(attempts[0].finished_at(), Some(20));
    }

    #[test]
    fn task_load_includes_linked_attempts() {
        let ledger = SqliteExecutionLedger::open_in_memory().unwrap();
        let task = task();
        ledger.save_task(&task).unwrap();
        let attempt = attempt("attempt-1", "codex");
        ledger
            .save_attempt(task.id(), &attempt, None, None)
            .unwrap();
        let loaded = ledger.get_task(task.id()).unwrap().unwrap();
        assert_eq!(loaded.attempts(), &[attempt]);
    }

    #[test]
    fn file_ledger_survives_connection_reopen() {
        let path = std::env::temp_dir().join(format!(
            "ai-dev-orchestrator-ledger-{}.sqlite",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let task = task();
        let attempt = attempt("attempt-1", "codex");
        {
            let ledger = SqliteExecutionLedger::open(&path).unwrap();
            ledger.save_task(&task).unwrap();
            ledger
                .save_attempt(task.id(), &attempt, Some(100), Some(200))
                .unwrap();
        }
        let reopened = SqliteExecutionLedger::open(&path).unwrap();
        let record = reopened
            .get_attempt(task.id(), attempt.id())
            .unwrap()
            .unwrap();
        assert_eq!(record.attempt(), &attempt);
        assert_eq!(record.started_at(), Some(100));
        assert_eq!(record.finished_at(), Some(200));
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn legacy_schema_migration_preserves_attempt_and_publication_data() {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(
                "CREATE TABLE tasks (id TEXT PRIMARY KEY NOT NULL, description TEXT NOT NULL, role TEXT NOT NULL, state TEXT NOT NULL);
                 CREATE TABLE attempts (task_id TEXT NOT NULL, id TEXT NOT NULL, provider TEXT NOT NULL, state TEXT NOT NULL,
                     started_at INTEGER, finished_at INTEGER, failure_reason TEXT, PRIMARY KEY (task_id, id));
                 CREATE TABLE publications (idempotency_key TEXT PRIMARY KEY NOT NULL, repository TEXT NOT NULL,
                     branch TEXT NOT NULL, commit_sha TEXT, pull_request TEXT, phase TEXT NOT NULL);
                 INSERT INTO tasks VALUES ('task-1', 'legacy', 'developer', 'failed');
                 INSERT INTO attempts VALUES ('task-1', 'attempt-1', 'codex', 'failed', 10, 20, 'timeout');
                 INSERT INTO publications VALUES ('key-1', 'owner/repo', 'main', 'abc', '42', 'published');
                 PRAGMA user_version = 1;",
            )
            .unwrap();
        let ledger = SqliteExecutionLedger::from_connection(connection).unwrap();

        let attempt = ledger
            .get_attempt(&TaskId::new("task-1"), &AttemptId::new("attempt-1"))
            .unwrap()
            .unwrap();
        assert_eq!(
            attempt.attempt().failure_reason(),
            Some(&AttemptFailureReason::Timeout)
        );
        let publication = ledger.load_publication("key-1").unwrap().unwrap();
        assert_eq!(publication.repository(), "owner/repo");
        assert_eq!(publication.branch(), "main");
        assert_eq!(publication.commit_sha(), Some("abc"));
        assert_eq!(publication.pull_request(), Some("42"));
        assert_eq!(publication.task_id(), None);
        assert_eq!(publication.workspace(), None);
        assert_eq!(publication.base(), None);
        assert_eq!(publication.title(), None);
        assert_eq!(publication.body(), None);
    }

    #[test]
    fn future_schema_version_is_rejected() {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch("PRAGMA user_version = 7;")
            .unwrap();
        assert!(matches!(
            SqliteExecutionLedger::from_connection(connection),
            Err(LedgerError::UnsupportedSchemaVersion(7))
        ));
    }
}
