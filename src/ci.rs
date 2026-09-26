//! Read-only CI observation and aggregation for a pinned GitHub target.

use std::{
    collections::HashSet,
    fmt,
    sync::atomic::{AtomicU64, Ordering},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use serde_json::Value;

use crate::{LedgerError, SqliteExecutionLedger, TaskId};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CiQueryTarget {
    PullRequest {
        repository: String,
        number: u64,
        expected_head_sha: Option<String>,
    },
    Commit {
        repository: String,
        sha: String,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CiTarget {
    pub(crate) repository: String,
    pub(crate) pull_request_number: Option<u64>,
    pub(crate) head_sha: String,
}

impl CiTarget {
    pub fn new(
        repository: impl Into<String>,
        pull_request_number: Option<u64>,
        head_sha: impl Into<String>,
    ) -> Self {
        Self {
            repository: repository.into(),
            pull_request_number,
            head_sha: head_sha.into(),
        }
    }
    pub fn repository(&self) -> &str {
        &self.repository
    }
    pub const fn pull_request_number(&self) -> Option<u64> {
        self.pull_request_number
    }
    pub fn head_sha(&self) -> &str {
        &self.head_sha
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CiAggregateState {
    Pending,
    Passed,
    Failed,
    Unknown,
}

impl CiAggregateState {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Passed => "passed",
            Self::Failed => "failed",
            Self::Unknown => "unknown",
        }
    }
    pub(crate) fn parse(value: &str) -> Result<Self, LedgerError> {
        match value {
            "pending" => Ok(Self::Pending),
            "passed" => Ok(Self::Passed),
            "failed" => Ok(Self::Failed),
            "unknown" => Ok(Self::Unknown),
            other => Err(LedgerError::InvalidStoredValue(format!(
                "CI state: {other}"
            ))),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CiCheckState {
    Pending,
    Passed,
    Failed,
    Unknown,
}
impl CiCheckState {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Passed => "passed",
            Self::Failed => "failed",
            Self::Unknown => "unknown",
        }
    }
    pub(crate) fn parse(value: &str) -> Result<Self, LedgerError> {
        match value {
            "pending" => Ok(Self::Pending),
            "passed" => Ok(Self::Passed),
            "failed" => Ok(Self::Failed),
            "unknown" => Ok(Self::Unknown),
            other => Err(LedgerError::InvalidStoredValue(format!(
                "CI check state: {other}"
            ))),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CiCheckDetailState {
    NotRegistered,
    Pending,
    Passed,
    Failed,
    Cancelled,
    Unavailable,
    Unknown,
}
impl CiCheckDetailState {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::NotRegistered => "not_registered",
            Self::Pending => "pending",
            Self::Passed => "passed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Unavailable => "unavailable",
            Self::Unknown => "unknown",
        }
    }
    pub(crate) fn parse(value: &str) -> Result<Self, LedgerError> {
        match value {
            "not_registered" => Ok(Self::NotRegistered),
            "pending" => Ok(Self::Pending),
            "passed" => Ok(Self::Passed),
            "failed" => Ok(Self::Failed),
            "cancelled" => Ok(Self::Cancelled),
            "unavailable" => Ok(Self::Unavailable),
            "unknown" => Ok(Self::Unknown),
            other => Err(LedgerError::InvalidStoredValue(format!(
                "CI detail state: {other}"
            ))),
        }
    }
    const fn compatible(self) -> CiCheckState {
        match self {
            Self::Pending => CiCheckState::Pending,
            Self::Passed => CiCheckState::Passed,
            Self::Failed | Self::Cancelled => CiCheckState::Failed,
            Self::NotRegistered | Self::Unavailable | Self::Unknown => CiCheckState::Unknown,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CiCheckSource {
    GithubCheckRuns,
    GithubCommitStatuses,
    Unknown,
}
impl CiCheckSource {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::GithubCheckRuns => "github_check_runs",
            Self::GithubCommitStatuses => "github_commit_statuses",
            Self::Unknown => "unknown",
        }
    }
    pub(crate) fn parse(value: &str) -> Result<Self, LedgerError> {
        match value {
            "github_check_runs" => Ok(Self::GithubCheckRuns),
            "github_commit_statuses" => Ok(Self::GithubCommitStatuses),
            "unknown" => Ok(Self::Unknown),
            other => Err(LedgerError::InvalidStoredValue(format!(
                "CI source: {other}"
            ))),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RequiredCheckSetState {
    Known,
    Unknown,
}
impl RequiredCheckSetState {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Known => "known",
            Self::Unknown => "unknown",
        }
    }
    pub(crate) fn parse(value: &str) -> Result<Self, LedgerError> {
        match value {
            "known" => Ok(Self::Known),
            "unknown" => Ok(Self::Unknown),
            other => Err(LedgerError::InvalidStoredValue(format!(
                "required check set state: {other}"
            ))),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RequiredCheckSetSource {
    GithubRuleset,
    TrustedConfiguration,
    Unknown,
}
impl RequiredCheckSetSource {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::GithubRuleset => "github_ruleset",
            Self::TrustedConfiguration => "trusted_configuration",
            Self::Unknown => "unknown",
        }
    }
    pub(crate) fn parse(value: &str) -> Result<Self, LedgerError> {
        match value {
            "github_ruleset" => Ok(Self::GithubRuleset),
            "trusted_configuration" => Ok(Self::TrustedConfiguration),
            "unknown" => Ok(Self::Unknown),
            other => Err(LedgerError::InvalidStoredValue(format!(
                "required check source: {other}"
            ))),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RequiredCheck {
    pub(crate) name: String,
    pub(crate) app_id: Option<i64>,
}
impl RequiredCheck {
    pub fn new(name: impl Into<String>, app_id: Option<i64>) -> Self {
        Self {
            name: name.into(),
            app_id,
        }
    }
    pub fn name(&self) -> &str {
        &self.name
    }
    pub const fn app_id(&self) -> Option<i64> {
        self.app_id
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RequiredCheckSet {
    pub(crate) state: RequiredCheckSetState,
    pub(crate) checks: Vec<RequiredCheck>,
    pub(crate) source: RequiredCheckSetSource,
    pub(crate) observed_at_ms: Option<i64>,
}
impl RequiredCheckSet {
    pub fn known(checks: Vec<RequiredCheck>, observed_at_ms: i64) -> Self {
        Self::known_from(
            checks,
            RequiredCheckSetSource::GithubRuleset,
            observed_at_ms,
        )
    }
    pub fn known_from(
        checks: Vec<RequiredCheck>,
        source: RequiredCheckSetSource,
        observed_at_ms: i64,
    ) -> Self {
        Self {
            state: RequiredCheckSetState::Known,
            checks,
            source,
            observed_at_ms: Some(observed_at_ms),
        }
    }
    pub fn unknown(source: RequiredCheckSetSource, observed_at_ms: Option<i64>) -> Self {
        Self {
            state: RequiredCheckSetState::Unknown,
            checks: Vec::new(),
            source,
            observed_at_ms,
        }
    }
    pub const fn state(&self) -> RequiredCheckSetState {
        self.state
    }
    pub fn checks(&self) -> &[RequiredCheck] {
        &self.checks
    }
    pub const fn source(&self) -> RequiredCheckSetSource {
        self.source
    }
    pub const fn observed_at_ms(&self) -> Option<i64> {
        self.observed_at_ms
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CiCheck {
    pub(crate) name: String,
    pub(crate) state: CiCheckState,
    pub(crate) detail_state: CiCheckDetailState,
    pub(crate) url: Option<String>,
    pub(crate) completed_at: Option<String>,
    pub(crate) required: Option<bool>,
    pub(crate) app_id: Option<i64>,
    pub(crate) source: CiCheckSource,
}
impl CiCheck {
    fn new(
        name: String,
        detail_state: CiCheckDetailState,
        url: Option<String>,
        completed_at: Option<String>,
        app_id: Option<i64>,
        source: CiCheckSource,
        required: Option<bool>,
    ) -> Self {
        Self {
            name,
            state: detail_state.compatible(),
            detail_state,
            url,
            completed_at,
            required,
            app_id,
            source,
        }
    }
    pub fn name(&self) -> &str {
        &self.name
    }
    pub const fn state(&self) -> CiCheckState {
        self.state
    }
    pub const fn detail_state(&self) -> CiCheckDetailState {
        self.detail_state
    }
    pub fn url(&self) -> Option<&str> {
        self.url.as_deref()
    }
    pub fn completed_at(&self) -> Option<&str> {
        self.completed_at.as_deref()
    }
    pub const fn required(&self) -> Option<bool> {
        self.required
    }
    pub const fn app_id(&self) -> Option<i64> {
        self.app_id
    }
    pub const fn source(&self) -> CiCheckSource {
        self.source
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CiObservation {
    pub(crate) id: String,
    pub(crate) task_id: Option<TaskId>,
    pub(crate) target: CiTarget,
    pub(crate) observed_at_ms: i64,
    pub(crate) state: CiAggregateState,
    pub(crate) checks: Vec<CiCheck>,
    pub(crate) required_checks: RequiredCheckSet,
}
impl CiObservation {
    pub fn id(&self) -> &str {
        &self.id
    }
    pub fn task_id(&self) -> Option<&TaskId> {
        self.task_id.as_ref()
    }
    pub fn target(&self) -> &CiTarget {
        &self.target
    }
    pub const fn observed_at_ms(&self) -> i64 {
        self.observed_at_ms
    }
    pub const fn state(&self) -> CiAggregateState {
        self.state
    }
    pub fn checks(&self) -> &[CiCheck] {
        &self.checks
    }
    pub fn required_checks(&self) -> &RequiredCheckSet {
        &self.required_checks
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RawCiCheck {
    pub name: String,
    pub detail_state: CiCheckDetailState,
    pub url: Option<String>,
    pub completed_at: Option<String>,
    pub app_id: Option<i64>,
    pub source: CiCheckSource,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CiProviderSnapshot {
    pub target: CiTarget,
    pub required_checks: RequiredCheckSet,
    pub checks: Vec<RawCiCheck>,
    pub check_runs_available: bool,
    pub commit_statuses_available: bool,
    pub observed_at_ms: i64,
}

pub trait CiProvider {
    fn observe(
        &self,
        target: &CiQueryTarget,
        timeout: Duration,
    ) -> Result<CiProviderSnapshot, CiProviderError>;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CiProviderError {
    Unavailable(String),
    InvalidResponse(String),
}

impl fmt::Display for CiProviderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unavailable(message) => write!(f, "CI observation unavailable: {message}"),
            Self::InvalidResponse(message) => write!(f, "invalid GitHub CI response: {message}"),
        }
    }
}
impl std::error::Error for CiProviderError {}

#[derive(Debug)]
pub enum CiError {
    Provider(CiProviderError),
    Ledger(LedgerError),
    InvalidTarget(String),
    HeadShaMismatch {
        expected: String,
        actual: String,
    },
    HeadChanged {
        expected: String,
        actual: String,
        last_observation_id: String,
    },
    Unavailable {
        reason: String,
        last_observation_id: Option<String>,
    },
    Timeout {
        last_observation_id: Option<String>,
    },
}
impl fmt::Display for CiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Provider(error) => error.fmt(f),
            Self::Ledger(error) => error.fmt(f),
            Self::InvalidTarget(reason) => write!(f, "invalid CI target: {reason}"),
            Self::HeadShaMismatch { expected, actual } => write!(
                f,
                "PR head SHA mismatch: expected {expected}, found {actual}"
            ),
            Self::HeadChanged {
                expected,
                actual,
                last_observation_id,
            } => write!(
                f,
                "PR head changed from {expected} to {actual}; last observation {last_observation_id}"
            ),
            Self::Unavailable {
                reason,
                last_observation_id: Some(id),
            } => write!(
                f,
                "CI observation unavailable: {reason}; last observation {id}"
            ),
            Self::Unavailable {
                reason,
                last_observation_id: None,
            } => write!(f, "CI observation unavailable: {reason}"),
            Self::Timeout {
                last_observation_id: Some(id),
            } => write!(f, "CI wait deadline elapsed; last observation {id}"),
            Self::Timeout {
                last_observation_id: None,
            } => f.write_str("CI wait deadline elapsed before the first observation"),
        }
    }
}
impl std::error::Error for CiError {}
impl From<LedgerError> for CiError {
    fn from(error: LedgerError) -> Self {
        Self::Ledger(error)
    }
}

pub struct CiRuntime<'a, P> {
    ledger: &'a SqliteExecutionLedger,
    provider: &'a P,
    poll_interval: Duration,
    api_timeout: Duration,
}
impl<'a, P: CiProvider> CiRuntime<'a, P> {
    pub fn new(
        ledger: &'a SqliteExecutionLedger,
        provider: &'a P,
        poll_interval: Duration,
        api_timeout: Duration,
    ) -> Result<Self, CiError> {
        if poll_interval.is_zero() || api_timeout.is_zero() {
            return Err(CiError::InvalidTarget(
                "poll and API timeouts must be positive".into(),
            ));
        }
        Ok(Self {
            ledger,
            provider,
            poll_interval,
            api_timeout,
        })
    }
    pub fn observe(
        &self,
        task_id: Option<&TaskId>,
        query: &CiQueryTarget,
    ) -> Result<CiObservation, CiError> {
        self.observe_until(task_id, query, self.api_timeout)
    }
    fn observe_until(
        &self,
        task_id: Option<&TaskId>,
        query: &CiQueryTarget,
        timeout: Duration,
    ) -> Result<CiObservation, CiError> {
        validate_query_target(query)?;
        let snapshot = self
            .provider
            .observe(query, timeout)
            .map_err(CiError::Provider)?;
        let target_matches = match query {
            CiQueryTarget::PullRequest {
                repository, number, ..
            } => {
                snapshot.target.repository == *repository
                    && snapshot.target.pull_request_number == Some(*number)
            }
            CiQueryTarget::Commit { repository, sha } => {
                snapshot.target.repository == *repository
                    && snapshot.target.pull_request_number.is_none()
                    && snapshot.target.head_sha == *sha
            }
        };
        if !target_matches || !valid_sha(&snapshot.target.head_sha) {
            return Err(CiError::Provider(CiProviderError::InvalidResponse(
                "GitHub adapter returned a different or invalid target".into(),
            )));
        }
        if let CiQueryTarget::PullRequest {
            expected_head_sha: Some(expected),
            ..
        } = query
        {
            if expected.as_str() != snapshot.target.head_sha {
                return Err(CiError::HeadShaMismatch {
                    expected: expected.clone(),
                    actual: snapshot.target.head_sha,
                });
            }
        }
        let (checks, state) = aggregate(&snapshot);
        static NEXT_ID: AtomicU64 = AtomicU64::new(0);
        let observation = CiObservation {
            id: format!(
                "ci-{}-{}-{}",
                snapshot.observed_at_ms,
                std::process::id(),
                NEXT_ID.fetch_add(1, Ordering::Relaxed)
            ),
            task_id: task_id.cloned(),
            target: snapshot.target,
            observed_at_ms: snapshot.observed_at_ms,
            state,
            checks,
            required_checks: snapshot.required_checks,
        };
        self.ledger.save_ci_observation(&observation)?;
        Ok(observation)
    }
    pub fn wait(
        &self,
        task_id: &TaskId,
        query: &CiQueryTarget,
        deadline: Instant,
    ) -> Result<CiObservation, CiError> {
        if deadline <= Instant::now() {
            return Err(CiError::Timeout {
                last_observation_id: None,
            });
        }
        let mut last: Option<CiObservation> = None;
        let mut pinned_sha: Option<String> = match query {
            CiQueryTarget::PullRequest {
                expected_head_sha, ..
            } => expected_head_sha.clone(),
            CiQueryTarget::Commit { sha, .. } => Some(sha.clone()),
        };
        loop {
            let now = Instant::now();
            if now >= deadline {
                return Err(CiError::Timeout {
                    last_observation_id: last.map(|observation| observation.id),
                });
            }
            let timeout = self
                .api_timeout
                .min(deadline.saturating_duration_since(now));
            let poll_query = match query {
                CiQueryTarget::PullRequest {
                    repository, number, ..
                } => CiQueryTarget::PullRequest {
                    repository: repository.clone(),
                    number: *number,
                    expected_head_sha: pinned_sha.clone(),
                },
                CiQueryTarget::Commit { .. } => query.clone(),
            };
            let observation = match self.observe_until(Some(task_id), &poll_query, timeout) {
                Ok(observation) => observation,
                Err(CiError::HeadShaMismatch { expected, actual })
                    if pinned_sha.as_deref() == Some(expected.as_str()) && last.is_some() =>
                {
                    return Err(CiError::HeadChanged {
                        expected,
                        actual,
                        last_observation_id: last
                            .map(|observation| observation.id)
                            .expect("checked above"),
                    });
                }
                Err(CiError::Provider(CiProviderError::Unavailable(reason))) => {
                    let last_observation_id = last.map(|observation| observation.id);
                    if Instant::now() >= deadline {
                        return Err(CiError::Timeout {
                            last_observation_id,
                        });
                    }
                    return Err(CiError::Unavailable {
                        reason,
                        last_observation_id,
                    });
                }
                Err(error) => return Err(error),
            };
            if let Some(expected) = pinned_sha.as_ref() {
                if expected.as_str() != observation.target.head_sha {
                    return Err(CiError::HeadChanged {
                        expected: expected.clone(),
                        actual: observation.target.head_sha,
                        last_observation_id: last
                            .map_or_else(|| observation.id.clone(), |previous| previous.id),
                    });
                }
            } else {
                pinned_sha = Some(observation.target.head_sha.clone());
            }
            let terminal = matches!(
                observation.state,
                CiAggregateState::Passed | CiAggregateState::Failed
            );
            let observation_id = observation.id.clone();
            if terminal {
                return Ok(observation);
            }
            last = Some(observation);
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(CiError::Timeout {
                    last_observation_id: Some(observation_id),
                });
            }
            thread::sleep(self.poll_interval.min(remaining));
        }
    }
}

fn validate_query_target(query: &CiQueryTarget) -> Result<(), CiError> {
    let (repository, sha) = match query {
        CiQueryTarget::PullRequest {
            repository,
            number,
            expected_head_sha,
        } => {
            if *number == 0 {
                return Err(CiError::InvalidTarget(
                    "pull request number must be positive".into(),
                ));
            }
            if expected_head_sha
                .as_ref()
                .is_some_and(|sha| !valid_sha(sha))
            {
                return Err(CiError::InvalidTarget(
                    "expected head must be a full Git object ID".into(),
                ));
            }
            (repository, None)
        }
        CiQueryTarget::Commit { repository, sha } => (repository, Some(sha)),
    };
    let mut parts = repository.split('/');
    let owner = parts.next().unwrap_or_default();
    let name = parts.next().unwrap_or_default();
    if parts.next().is_some()
        || owner.is_empty()
        || name.is_empty()
        || ![owner, name].iter().all(|part| {
            part.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        })
    {
        return Err(CiError::InvalidTarget(
            "repository must be owner/repo".into(),
        ));
    }
    if sha.is_some_and(|sha| !valid_sha(sha)) {
        return Err(CiError::InvalidTarget(
            "commit target must be a full Git object ID".into(),
        ));
    }
    Ok(())
}
fn valid_sha(sha: &str) -> bool {
    (sha.len() == 40 || sha.len() == 64) && sha.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn aggregate(snapshot: &CiProviderSnapshot) -> (Vec<CiCheck>, CiAggregateState) {
    let known_required = snapshot.required_checks.state == RequiredCheckSetState::Known;
    let mut checks: Vec<CiCheck> = snapshot
        .checks
        .iter()
        .map(|raw| {
            CiCheck::new(
                raw.name.clone(),
                raw.detail_state,
                raw.url.clone(),
                raw.completed_at.clone(),
                raw.app_id,
                raw.source,
                if known_required { Some(false) } else { None },
            )
        })
        .collect();
    if !known_required || snapshot.required_checks.checks.is_empty() {
        return (checks, CiAggregateState::Unknown);
    }
    let mut required_states = Vec::new();
    for required in &snapshot.required_checks.checks {
        let eligible_sources_complete = if required.app_id.is_some() {
            snapshot.check_runs_available
        } else {
            snapshot.check_runs_available && snapshot.commit_statuses_available
        };
        let same_name: Vec<usize> = snapshot
            .checks
            .iter()
            .enumerate()
            .filter_map(|(index, check)| (check.name == required.name).then_some(index))
            .collect();
        let candidates: Vec<usize> = same_name
            .iter()
            .copied()
            .filter(|index| {
                required
                    .app_id
                    .is_none_or(|app| snapshot.checks[*index].app_id == Some(app))
            })
            .collect();
        if candidates.len() == 1 && eligible_sources_complete {
            let index = candidates[0];
            checks[index].required = Some(true);
            required_states.push(snapshot.checks[index].detail_state);
        } else if candidates.is_empty() {
            let detail = if !same_name.is_empty() {
                CiCheckDetailState::Unknown
            } else if eligible_sources_complete {
                CiCheckDetailState::NotRegistered
            } else {
                CiCheckDetailState::Unavailable
            };
            checks.push(CiCheck::new(
                required.name.clone(),
                detail,
                None,
                None,
                required.app_id,
                CiCheckSource::Unknown,
                Some(true),
            ));
            required_states.push(detail);
        } else {
            checks.push(CiCheck::new(
                required.name.clone(),
                CiCheckDetailState::Unknown,
                None,
                None,
                required.app_id,
                CiCheckSource::Unknown,
                Some(true),
            ));
            required_states.push(CiCheckDetailState::Unknown);
        }
    }
    let state = if required_states.iter().any(|state| {
        matches!(
            state,
            CiCheckDetailState::Failed | CiCheckDetailState::Cancelled
        )
    }) {
        CiAggregateState::Failed
    } else if required_states.iter().any(|state| {
        matches!(
            state,
            CiCheckDetailState::Unavailable | CiCheckDetailState::Unknown
        )
    }) {
        CiAggregateState::Unknown
    } else if required_states.iter().any(|state| {
        matches!(
            state,
            CiCheckDetailState::Pending | CiCheckDetailState::NotRegistered
        )
    }) {
        CiAggregateState::Pending
    } else if required_states
        .iter()
        .all(|state| *state == CiCheckDetailState::Passed)
    {
        CiAggregateState::Passed
    } else {
        CiAggregateState::Unknown
    };
    (checks, state)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GhCiProvider {
    executable: std::path::PathBuf,
}
impl Default for GhCiProvider {
    fn default() -> Self {
        Self::new()
    }
}
impl GhCiProvider {
    pub fn new() -> Self {
        Self {
            executable: "gh".into(),
        }
    }
    pub fn with_executable(executable: impl Into<std::path::PathBuf>) -> Self {
        Self {
            executable: executable.into(),
        }
    }
}

impl CiProvider for GhCiProvider {
    fn observe(
        &self,
        query: &CiQueryTarget,
        timeout: Duration,
    ) -> Result<CiProviderSnapshot, CiProviderError> {
        let now = unix_ms();
        let deadline = Instant::now() + timeout;
        let (repository, pr_number, sha, base_branch) = match query {
            CiQueryTarget::Commit { repository, sha } => {
                (repository.clone(), None, sha.clone(), None)
            }
            CiQueryTarget::PullRequest {
                repository, number, ..
            } => {
                let pr_result = remaining_timeout(deadline).and_then(|remaining| {
                    self.api_json(&format!("repos/{repository}/pulls/{number}"), remaining)
                });
                match pr_result {
                    Ok(value) => {
                        let pr = page_values(&value).first().copied().ok_or_else(|| {
                            CiProviderError::InvalidResponse("PR response was empty".into())
                        })?;
                        let sha = pr
                            .pointer("/head/sha")
                            .and_then(Value::as_str)
                            .ok_or_else(|| {
                                CiProviderError::InvalidResponse(
                                    "PR response omitted head SHA".into(),
                                )
                            })?
                            .to_owned();
                        let base = pr
                            .pointer("/base/ref")
                            .and_then(Value::as_str)
                            .ok_or_else(|| {
                                CiProviderError::InvalidResponse(
                                    "PR response omitted base branch".into(),
                                )
                            })?
                            .to_owned();
                        (repository.clone(), Some(*number), sha, Some(base))
                    }
                    Err(error) => return Err(error),
                }
            }
        };
        let required_checks = if let Some(branch) = base_branch.as_deref() {
            let protection = remaining_timeout(deadline).and_then(|remaining| {
                self.api_status_json(
                    &format!(
                        "repos/{repository}/branches/{}/protection",
                        encode_path_segment(branch)
                    ),
                    remaining,
                )
            });
            required_check_set_from_sources(protection, || {
                self.ruleset_required_checks(&repository, branch, now, deadline)
            })
        } else {
            RequiredCheckSet::unknown(RequiredCheckSetSource::Unknown, None)
        };
        let runs_result = remaining_timeout(deadline).and_then(|remaining| {
            self.api_json(
                &format!("repos/{repository}/commits/{sha}/check-runs?per_page=100"),
                remaining,
            )
        });
        let statuses_result = remaining_timeout(deadline).and_then(|remaining| {
            self.api_json(
                &format!("repos/{repository}/commits/{sha}/statuses?per_page=100"),
                remaining,
            )
        });
        let mut checks = Vec::new();
        let check_runs_available = match runs_result.and_then(|value| parse_check_runs(&value)) {
            Ok(runs) => {
                checks.extend(runs);
                true
            }
            Err(_) => false,
        };
        let commit_statuses_available =
            match statuses_result.and_then(|value| parse_statuses(&value)) {
                Ok(statuses) => {
                    checks.extend(statuses);
                    true
                }
                Err(_) => false,
            };
        Ok(CiProviderSnapshot {
            target: CiTarget {
                repository,
                pull_request_number: pr_number,
                head_sha: sha,
            },
            required_checks,
            checks,
            check_runs_available,
            commit_statuses_available,
            observed_at_ms: now,
        })
    }
}

impl GhCiProvider {
    fn ruleset_required_checks(
        &self,
        repository: &str,
        branch: &str,
        observed_at_ms: i64,
        deadline: Instant,
    ) -> Result<RequiredCheckSet, CiProviderError> {
        let rules = self.api_json(
            &format!(
                "repos/{repository}/rules/branches/{}?per_page=100",
                encode_path_segment(branch)
            ),
            remaining_timeout(deadline)?,
        )?;
        Ok(RequiredCheckSet::known(
            parse_rules(&rules)?,
            observed_at_ms,
        ))
    }

    fn api_status_json(
        &self,
        endpoint: &str,
        timeout: Duration,
    ) -> Result<Option<Value>, CiProviderError> {
        let output = crate::ProcessRunner
            .run(
                crate::ProcessRequest::new(self.executable.as_os_str().to_owned())
                    .args(["api", "--include", endpoint])
                    .timeout(timeout),
            )
            .map_err(|_| {
                CiProviderError::Unavailable("GitHub API command did not complete".into())
            })?;
        let response = String::from_utf8_lossy(&output.stdout);
        let (headers, body) = response
            .split_once("\r\n\r\n")
            .or_else(|| response.split_once("\n\n"))
            .ok_or_else(|| {
                CiProviderError::InvalidResponse("GitHub API response omitted headers".into())
            })?;
        let status = headers
            .lines()
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
            .and_then(|code| code.parse::<u16>().ok())
            .ok_or_else(|| {
                CiProviderError::InvalidResponse("GitHub API response omitted status".into())
            })?;
        match status {
            200 => serde_json::from_str(body)
                .map(Some)
                .map_err(|error| CiProviderError::InvalidResponse(error.to_string())),
            404 => Ok(None),
            _ => Err(CiProviderError::Unavailable(format!(
                "GitHub branch protection API returned HTTP {status}"
            ))),
        }
    }
}

fn has_classic_required_checks(protection: &Value) -> Result<bool, CiProviderError> {
    let required = protection.get("required_status_checks").ok_or_else(|| {
        CiProviderError::InvalidResponse("branch protection omitted required_status_checks".into())
    })?;
    if required.is_null() {
        return Ok(false);
    }
    let object = required.as_object().ok_or_else(|| {
        CiProviderError::InvalidResponse("required_status_checks was not an object".into())
    })?;
    let contexts = object
        .get("contexts")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            CiProviderError::InvalidResponse("required_status_checks omitted contexts".into())
        })?;
    let checks = object
        .get("checks")
        .and_then(Value::as_array)
        .map_or(&[][..], Vec::as_slice);
    Ok(!contexts.is_empty() || !checks.is_empty())
}

fn classic_protection_confirms_no_required_checks(
    response: Result<Option<Value>, CiProviderError>,
) -> Result<Option<bool>, CiProviderError> {
    match response {
        Ok(Some(protection)) => {
            has_classic_required_checks(&protection).map(|has_checks| Some(!has_checks))
        }
        Ok(None) => Ok(None),
        Err(error) => Err(error),
    }
}

fn required_check_set_from_sources(
    protection: Result<Option<Value>, CiProviderError>,
    load_ruleset: impl FnOnce() -> Result<RequiredCheckSet, CiProviderError>,
) -> RequiredCheckSet {
    match classic_protection_confirms_no_required_checks(protection) {
        Ok(Some(true)) => load_ruleset().unwrap_or_else(|_| {
            RequiredCheckSet::unknown(RequiredCheckSetSource::GithubRuleset, None)
        }),
        Ok(Some(false) | None) | Err(_) => {
            RequiredCheckSet::unknown(RequiredCheckSetSource::Unknown, None)
        }
    }
}

impl GhCiProvider {
    fn api_json(&self, endpoint: &str, timeout: Duration) -> Result<Value, CiProviderError> {
        let output = crate::ProcessRunner
            .run(
                crate::ProcessRequest::new(self.executable.as_os_str().to_owned())
                    .args(["api", "--paginate", "--slurp", endpoint])
                    .timeout(timeout),
            )
            .map_err(|_| {
                CiProviderError::Unavailable("GitHub API command did not complete".into())
            })?;
        if output.exit_code() != Some(0) {
            return Err(CiProviderError::Unavailable(
                "GitHub API request failed".into(),
            ));
        }
        serde_json::from_slice(&output.stdout)
            .map_err(|error| CiProviderError::InvalidResponse(error.to_string()))
    }
}

fn parse_rules(value: &Value) -> Result<Vec<RequiredCheck>, CiProviderError> {
    let pages = page_values(value);
    let mut required = Vec::new();
    for page in pages {
        let rules = page.as_array().ok_or_else(|| {
            CiProviderError::InvalidResponse("branch rules response was not an array".into())
        })?;
        for rule in rules {
            if rule.get("type").and_then(Value::as_str) != Some("required_status_checks") {
                continue;
            }
            let entries = rule
                .pointer("/parameters/required_status_checks")
                .and_then(Value::as_array)
                .ok_or_else(|| {
                    CiProviderError::InvalidResponse(
                        "required_status_checks rule omitted parameters".into(),
                    )
                })?;
            for entry in entries {
                let name = entry
                    .get("context")
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        CiProviderError::InvalidResponse(
                            "required status check omitted context".into(),
                        )
                    })?;
                let app_id = entry.get("integration_id").and_then(Value::as_i64);
                let check = RequiredCheck::new(name, app_id);
                if !required.contains(&check) {
                    required.push(check);
                }
            }
        }
    }
    Ok(required)
}

fn parse_check_runs(value: &Value) -> Result<Vec<RawCiCheck>, CiProviderError> {
    let mut checks = Vec::new();
    for page in page_values(value) {
        let runs = page
            .get("check_runs")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                CiProviderError::InvalidResponse("check-runs response omitted check_runs".into())
            })?;
        for run in runs {
            let name = run
                .get("name")
                .and_then(Value::as_str)
                .ok_or_else(|| CiProviderError::InvalidResponse("check run omitted name".into()))?
                .to_owned();
            let status = run
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            let conclusion = run.get("conclusion").and_then(Value::as_str);
            let detail_state = match (status, conclusion) {
                ("queued" | "in_progress" | "pending" | "waiting" | "requested", _) => {
                    CiCheckDetailState::Pending
                }
                ("completed", Some("success" | "neutral" | "skipped")) => {
                    CiCheckDetailState::Passed
                }
                ("completed", Some("cancelled")) => CiCheckDetailState::Cancelled,
                (
                    "completed",
                    Some("failure" | "timed_out" | "action_required" | "startup_failure" | "stale"),
                ) => CiCheckDetailState::Failed,
                ("completed", _) => CiCheckDetailState::Unknown,
                _ => CiCheckDetailState::Unknown,
            };
            checks.push(RawCiCheck {
                name,
                detail_state,
                url: run
                    .get("html_url")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                completed_at: run
                    .get("completed_at")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                app_id: run.pointer("/app/id").and_then(Value::as_i64),
                source: CiCheckSource::GithubCheckRuns,
            });
        }
    }
    Ok(checks)
}

fn parse_statuses(value: &Value) -> Result<Vec<RawCiCheck>, CiProviderError> {
    let mut checks = Vec::new();
    let mut seen_contexts = HashSet::new();
    for page in page_values(value) {
        let statuses = page.as_array().ok_or_else(|| {
            CiProviderError::InvalidResponse("commit statuses response was not an array".into())
        })?;
        for status in statuses {
            let name = status
                .get("context")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    CiProviderError::InvalidResponse("commit status omitted context".into())
                })?
                .to_owned();
            if !seen_contexts.insert(name.clone()) {
                continue;
            }
            let detail_state = match status
                .get("state")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
            {
                "pending" => CiCheckDetailState::Pending,
                "success" => CiCheckDetailState::Passed,
                "failure" | "error" => CiCheckDetailState::Failed,
                _ => CiCheckDetailState::Unknown,
            };
            checks.push(RawCiCheck {
                name,
                detail_state,
                url: status
                    .get("target_url")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                completed_at: None,
                app_id: None,
                source: CiCheckSource::GithubCommitStatuses,
            });
        }
    }
    Ok(checks)
}

fn remaining_timeout(deadline: Instant) -> Result<Duration, CiProviderError> {
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        Err(CiProviderError::Unavailable(
            "GitHub API observation deadline elapsed".into(),
        ))
    } else {
        Ok(remaining)
    }
}

fn page_values(value: &Value) -> Vec<&Value> {
    value
        .as_array()
        .map_or_else(|| vec![value], |pages| pages.iter().collect())
}

fn encode_path_segment(value: &str) -> String {
    let mut encoded = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            encoded.push(char::from(byte));
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

fn unix_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| {
            i64::try_from(duration.as_millis()).unwrap_or(i64::MAX)
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Task, TaskRole};
    use std::{collections::VecDeque, sync::Mutex};

    fn save_task(ledger: &SqliteExecutionLedger, id: &str) -> TaskId {
        let task_id = TaskId::new(id);
        ledger
            .save_task(&Task::new(
                task_id.clone(),
                "observe required CI checks",
                TaskRole::new("reviewer"),
            ))
            .unwrap();
        task_id
    }

    struct FakeProvider {
        responses: Mutex<VecDeque<Result<CiProviderSnapshot, CiProviderError>>>,
        fallback: CiProviderSnapshot,
        queries: Mutex<Vec<CiQueryTarget>>,
        unavailable_delay: Duration,
    }

    impl FakeProvider {
        fn new(snapshots: impl IntoIterator<Item = CiProviderSnapshot>) -> Self {
            let snapshots = snapshots.into_iter().collect::<VecDeque<_>>();
            let fallback = snapshots.back().expect("fixture snapshot").clone();
            Self {
                responses: Mutex::new(snapshots.into_iter().map(Ok).collect()),
                fallback,
                queries: Mutex::new(Vec::new()),
                unavailable_delay: Duration::ZERO,
            }
        }

        fn unavailable_after(first: CiProviderSnapshot, error: CiProviderError) -> Self {
            Self::unavailable_after_with_delay(first, error, Duration::ZERO)
        }

        fn unavailable_after_with_delay(
            first: CiProviderSnapshot,
            error: CiProviderError,
            delay: Duration,
        ) -> Self {
            Self {
                responses: Mutex::new(VecDeque::from([Ok(first.clone()), Err(error)])),
                fallback: first,
                queries: Mutex::new(Vec::new()),
                unavailable_delay: delay,
            }
        }
    }

    impl CiProvider for FakeProvider {
        fn observe(
            &self,
            target: &CiQueryTarget,
            _: Duration,
        ) -> Result<CiProviderSnapshot, CiProviderError> {
            self.queries.lock().unwrap().push(target.clone());
            let response = self
                .responses
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_else(|| Ok(self.fallback.clone()));
            if response.is_err() && !self.unavailable_delay.is_zero() {
                thread::sleep(self.unavailable_delay);
            }
            response
        }
    }

    fn snapshot(sha: &str, detail: CiCheckDetailState) -> CiProviderSnapshot {
        CiProviderSnapshot {
            target: CiTarget::new("owner/repo", Some(4), sha),
            required_checks: RequiredCheckSet::known(
                vec![RequiredCheck::new("build", Some(7))],
                123,
            ),
            checks: vec![RawCiCheck {
                name: "build".into(),
                detail_state: detail,
                url: Some("https://example.test/check".into()),
                completed_at: Some("2026-01-01T00:00:00Z".into()),
                app_id: Some(7),
                source: CiCheckSource::GithubCheckRuns,
            }],
            check_runs_available: true,
            commit_statuses_available: true,
            observed_at_ms: 123,
        }
    }

    #[test]
    fn known_empty_and_unknown_required_sets_never_pass() {
        for required_checks in [
            RequiredCheckSet::known(Vec::new(), 123),
            RequiredCheckSet::unknown(RequiredCheckSetSource::GithubRuleset, None),
        ] {
            let input = CiProviderSnapshot {
                target: CiTarget::new("owner/repo", Some(4), "a".repeat(40)),
                required_checks,
                checks: vec![],
                check_runs_available: true,
                commit_statuses_available: true,
                observed_at_ms: 123,
            };
            assert_eq!(aggregate(&input).1, CiAggregateState::Unknown);
        }
    }

    #[test]
    fn cancelled_required_check_fails_and_unknown_source_cannot_pass() {
        let sha = "a".repeat(40);
        assert_eq!(
            aggregate(&snapshot(&sha, CiCheckDetailState::Cancelled)).1,
            CiAggregateState::Failed
        );
        let mut incomplete = snapshot(&sha, CiCheckDetailState::Passed);
        incomplete.check_runs_available = false;
        assert_eq!(aggregate(&incomplete).1, CiAggregateState::Unknown);

        let mut mismatched_app = snapshot(&sha, CiCheckDetailState::Passed);
        mismatched_app.checks[0].app_id = Some(99);
        let (checks, state) = aggregate(&mismatched_app);
        assert_eq!(state, CiAggregateState::Unknown);
        assert_eq!(
            checks.last().unwrap().detail_state(),
            CiCheckDetailState::Unknown
        );

        let mut unregistered = snapshot(&sha, CiCheckDetailState::Passed);
        unregistered.checks.clear();
        let (checks, state) = aggregate(&unregistered);
        assert_eq!(state, CiAggregateState::Pending);
        assert_eq!(checks[0].detail_state(), CiCheckDetailState::NotRegistered);

        let mut unavailable = unregistered;
        unavailable.check_runs_available = false;
        let (checks, state) = aggregate(&unavailable);
        assert_eq!(state, CiAggregateState::Unknown);
        assert_eq!(checks[0].detail_state(), CiCheckDetailState::Unavailable);
    }

    #[test]
    fn classic_required_checks_cannot_be_mislabeled_as_rulesets() {
        let classic = serde_json::json!({
            "required_status_checks": {
                "contexts": ["legacy/build"],
                "checks": [{"context": "legacy/build", "app_id": 7}]
            }
        });
        assert_eq!(has_classic_required_checks(&classic), Ok(true));
        let no_classic_checks = serde_json::json!({
            "required_status_checks": {"contexts": [], "checks": []}
        });
        assert_eq!(has_classic_required_checks(&no_classic_checks), Ok(false));
        assert_eq!(
            classic_protection_confirms_no_required_checks(Ok(Some(no_classic_checks))),
            Ok(Some(true))
        );
        assert_eq!(
            classic_protection_confirms_no_required_checks(Ok(None)),
            Ok(None)
        );
        let absent = required_check_set_from_sources(Ok(None), || {
            Ok(RequiredCheckSet::known(
                vec![RequiredCheck::new("ruleset/build", Some(7))],
                123,
            ))
        });
        assert_eq!(absent.state(), RequiredCheckSetState::Unknown);
        assert_eq!(absent.source(), RequiredCheckSetSource::Unknown);
        let empty_classic = required_check_set_from_sources(
            Ok(Some(serde_json::json!({
                "required_status_checks": {"contexts": [], "checks": []}
            }))),
            || {
                Ok(RequiredCheckSet::known(
                    vec![RequiredCheck::new("ruleset/build", Some(7))],
                    123,
                ))
            },
        );
        assert_eq!(empty_classic.state(), RequiredCheckSetState::Known);
        assert_eq!(
            empty_classic.source(),
            RequiredCheckSetSource::GithubRuleset
        );
    }

    #[test]
    fn statuses_keep_only_the_newest_result_for_each_context() {
        let statuses = serde_json::json!([[
            {
                "context": "build",
                "state": "success",
                "target_url": "https://example.test/new"
            },
            {
                "context": "build",
                "state": "failure",
                "target_url": "https://example.test/old"
            }
        ]]);
        let parsed = parse_statuses(&statuses).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].detail_state, CiCheckDetailState::Passed);
        assert_eq!(parsed[0].url.as_deref(), Some("https://example.test/new"));
    }

    #[test]
    fn neutral_and_skipped_check_runs_are_passed_and_waiting_states_are_pending() {
        for conclusion in ["neutral", "skipped"] {
            let runs = serde_json::json!([{
                "check_runs": [{"name": "build", "status": "completed", "conclusion": conclusion}]
            }]);
            assert_eq!(
                parse_check_runs(&runs).unwrap()[0].detail_state,
                CiCheckDetailState::Passed
            );
        }
        for status in ["pending", "waiting", "requested"] {
            let runs = serde_json::json!([{
                "check_runs": [{"name": "build", "status": status, "conclusion": null}]
            }]);
            assert_eq!(
                parse_check_runs(&runs).unwrap()[0].detail_state,
                CiCheckDetailState::Pending
            );
        }
    }

    #[test]
    fn wait_timeout_returns_last_persisted_observation() {
        let ledger = SqliteExecutionLedger::open_in_memory().unwrap();
        let provider = FakeProvider::new([snapshot(&"a".repeat(40), CiCheckDetailState::Pending)]);
        let runtime = CiRuntime::new(
            &ledger,
            &provider,
            Duration::from_millis(1),
            Duration::from_millis(10),
        )
        .unwrap();
        let task_id = save_task(&ledger, "ci-timeout-task");
        let error = runtime
            .wait(
                &task_id,
                &CiQueryTarget::PullRequest {
                    repository: "owner/repo".into(),
                    number: 4,
                    expected_head_sha: None,
                },
                Instant::now() + Duration::from_millis(20),
            )
            .unwrap_err();
        let CiError::Timeout {
            last_observation_id: Some(id),
        } = error
        else {
            panic!("expected timeout with persisted observation: {error:?}");
        };
        let saved = ledger.get_ci_observation(&id).unwrap().unwrap();
        assert_eq!(saved.task_id(), Some(&task_id));
        assert_eq!(saved.state(), CiAggregateState::Pending);
    }

    #[test]
    fn wait_stops_when_pr_head_changes() {
        let ledger = SqliteExecutionLedger::open_in_memory().unwrap();
        save_task(&ledger, "ci-head-task");
        let provider = FakeProvider::new([
            snapshot(&"a".repeat(40), CiCheckDetailState::Pending),
            snapshot(&"b".repeat(40), CiCheckDetailState::Pending),
        ]);
        let runtime = CiRuntime::new(
            &ledger,
            &provider,
            Duration::from_millis(1),
            Duration::from_millis(10),
        )
        .unwrap();
        let error = runtime
            .wait(
                &TaskId::new("ci-head-task"),
                &CiQueryTarget::PullRequest {
                    repository: "owner/repo".into(),
                    number: 4,
                    expected_head_sha: None,
                },
                Instant::now() + Duration::from_secs(1),
            )
            .unwrap_err();
        let CiError::HeadChanged {
            expected,
            actual,
            last_observation_id,
        } = error
        else {
            panic!("expected a pinned-head change: {error:?}");
        };
        assert_eq!(expected, "a".repeat(40));
        assert_eq!(actual, "b".repeat(40));
        let last = ledger
            .get_ci_observation(&last_observation_id)
            .unwrap()
            .unwrap();
        assert_eq!(last.target().head_sha(), "a".repeat(40));
    }

    #[test]
    fn wait_stops_as_unavailable_when_pr_head_cannot_be_reconfirmed() {
        let ledger = SqliteExecutionLedger::open_in_memory().unwrap();
        let task_id = save_task(&ledger, "ci-unavailable-task");
        let first = snapshot(&"a".repeat(40), CiCheckDetailState::Pending);
        let provider = FakeProvider::unavailable_after(
            first,
            CiProviderError::Unavailable("PR details request failed".into()),
        );
        let runtime = CiRuntime::new(
            &ledger,
            &provider,
            Duration::from_millis(1),
            Duration::from_millis(10),
        )
        .unwrap();
        let error = runtime
            .wait(
                &task_id,
                &CiQueryTarget::PullRequest {
                    repository: "owner/repo".into(),
                    number: 4,
                    expected_head_sha: None,
                },
                Instant::now() + Duration::from_secs(1),
            )
            .unwrap_err();
        let CiError::Unavailable {
            reason,
            last_observation_id: Some(id),
        } = error
        else {
            panic!("expected unavailable with last observation: {error:?}");
        };
        assert_eq!(reason, "PR details request failed");
        let last = ledger.get_ci_observation(&id).unwrap().unwrap();
        assert_eq!(last.target().head_sha(), "a".repeat(40));
        assert!(matches!(
            provider.queries.lock().unwrap().get(1),
            Some(CiQueryTarget::PullRequest {
                expected_head_sha: Some(sha), ..
            }) if sha == &"a".repeat(40)
        ));
    }

    #[test]
    fn wait_returns_timeout_when_unavailable_response_arrives_after_deadline() {
        let ledger = SqliteExecutionLedger::open_in_memory().unwrap();
        let task_id = save_task(&ledger, "ci-timeout-unavailable-task");
        let provider = FakeProvider::unavailable_after_with_delay(
            snapshot(&"a".repeat(40), CiCheckDetailState::Pending),
            CiProviderError::Unavailable("PR details request failed".into()),
            Duration::from_millis(150),
        );
        let runtime = CiRuntime::new(
            &ledger,
            &provider,
            Duration::from_millis(1),
            Duration::from_secs(1),
        )
        .unwrap();
        let error = runtime
            .wait(
                &task_id,
                &CiQueryTarget::PullRequest {
                    repository: "owner/repo".into(),
                    number: 4,
                    expected_head_sha: None,
                },
                Instant::now() + Duration::from_millis(100),
            )
            .unwrap_err();
        let CiError::Timeout {
            last_observation_id: Some(id),
        } = error
        else {
            panic!("expected timeout with last observation: {error:?}");
        };
        let last = ledger.get_ci_observation(&id).unwrap().unwrap();
        assert_eq!(last.target().head_sha(), "a".repeat(40));
        assert_eq!(last.state(), CiAggregateState::Pending);
    }

    #[test]
    fn cancelled_observation_round_trips_through_ledger() {
        let ledger = SqliteExecutionLedger::open_in_memory().unwrap();
        let task_id = save_task(&ledger, "ci-roundtrip-task");
        let provider =
            FakeProvider::new([snapshot(&"a".repeat(40), CiCheckDetailState::Cancelled)]);
        let runtime = CiRuntime::new(
            &ledger,
            &provider,
            Duration::from_millis(1),
            Duration::from_secs(1),
        )
        .unwrap();
        let observation = runtime
            .observe(
                Some(&task_id),
                &CiQueryTarget::PullRequest {
                    repository: "owner/repo".into(),
                    number: 4,
                    expected_head_sha: Some("a".repeat(40)),
                },
            )
            .unwrap();
        let restored = ledger
            .get_ci_observation(observation.id())
            .unwrap()
            .unwrap();
        assert_eq!(restored, observation);
        assert_eq!(restored.state(), CiAggregateState::Failed);
        assert_eq!(
            restored.checks()[0].detail_state(),
            CiCheckDetailState::Cancelled
        );
        assert_eq!(restored.checks()[0].required(), Some(true));
    }
}
