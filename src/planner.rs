//! Provider selection by a Codex-backed planner.
//!
//! The planner is a semantic decision boundary.  It receives an immutable snapshot
//! of a task and provider availability facts, and returns intent only.  It never
//! receives a mutable `Task`, and therefore cannot bypass the domain state machine.

use std::{
    ffi::{OsStr, OsString},
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

use serde_json::{Value, json};

use crate::{
    ProcessError, ProcessRequest, ProcessRunner, ProviderRef, Task, TaskId, TaskRole, TaskState,
};

const CODEX_COMMAND: &str = "codex";
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60);

/// A provider and its availability as observed by Rust.
///
/// `available` is an input fact supplied by the orchestrator.  Planner output cannot
/// change it; a decision is checked against this list before it can be consumed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProviderAvailability {
    provider: ProviderRef,
    available: bool,
}

impl ProviderAvailability {
    #[must_use]
    pub fn new(provider: ProviderRef, available: bool) -> Self {
        Self {
            provider,
            available,
        }
    }

    #[must_use]
    pub fn available(provider: ProviderRef) -> Self {
        Self::new(provider, true)
    }

    #[must_use]
    pub fn unavailable(provider: ProviderRef) -> Self {
        Self::new(provider, false)
    }

    #[must_use]
    pub const fn provider(&self) -> &ProviderRef {
        &self.provider
    }

    #[must_use]
    pub const fn is_available(&self) -> bool {
        self.available
    }
}

/// Compatibility name for callers that call an availability entry a provider.
pub type PlannerProvider = ProviderAvailability;
/// Compatibility name emphasizing that the list is produced by Rust.
pub type AvailableProvider = ProviderAvailability;

/// Immutable planner input assembled from a Task and Rust provider facts.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlannerRequest {
    task_id: TaskId,
    description: String,
    role: TaskRole,
    state: TaskState,
    providers: Vec<ProviderAvailability>,
    timeout: Duration,
}

impl PlannerRequest {
    /// Builds a request without borrowing the Task after this call returns.
    #[must_use]
    pub fn from_task(
        task: &Task,
        providers: impl IntoIterator<Item = ProviderAvailability>,
    ) -> Self {
        Self {
            task_id: task.id().clone(),
            description: task.description().to_owned(),
            role: task.role().clone(),
            state: task.state(),
            providers: providers.into_iter().collect(),
            timeout: DEFAULT_TIMEOUT,
        }
    }

    /// Alias useful to callers that construct a request directly from a Task.
    #[must_use]
    pub fn new(task: &Task, providers: impl IntoIterator<Item = ProviderAvailability>) -> Self {
        Self::from_task(task, providers)
    }

    /// Builds a request from provider references, treating all of them as available.
    #[must_use]
    pub fn from_available_provider_refs(
        task: &Task,
        providers: impl IntoIterator<Item = ProviderRef>,
    ) -> Self {
        Self::from_task(
            task,
            providers.into_iter().map(ProviderAvailability::available),
        )
    }

    #[must_use]
    pub const fn task_id(&self) -> &TaskId {
        &self.task_id
    }

    #[must_use]
    pub fn description(&self) -> &str {
        &self.description
    }

    #[must_use]
    pub const fn role(&self) -> &TaskRole {
        &self.role
    }

    #[must_use]
    pub const fn state(&self) -> TaskState {
        self.state
    }

    /// All provider facts, including unavailable entries.
    #[must_use]
    pub fn providers(&self) -> &[ProviderAvailability] {
        &self.providers
    }

    /// Provider facts marked available by Rust.
    pub fn available_providers(&self) -> impl Iterator<Item = &ProviderAvailability> {
        self.providers.iter().filter(|provider| provider.available)
    }

    #[must_use]
    pub const fn timeout(&self) -> Duration {
        self.timeout
    }

    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Serializes the task and provider facts into the planner prompt.
    #[must_use]
    pub fn prompt(&self) -> String {
        let providers: Vec<_> = self
            .providers
            .iter()
            .map(|entry| {
                json!({
                    "provider": entry.provider.as_str(),
                    "available": entry.available,
                })
            })
            .collect();
        let input = json!({
            "task": {
                "id": self.task_id.as_str(),
                "description": self.description,
                "role": self.role.as_str(),
                "state": format!("{:?}", self.state),
            },
            "providers": providers,
        });
        format!(
            "Select a provider for this task. Return only JSON matching the output schema.\n\
             Provider availability is a Rust fact; select only an entry whose available is true.\n\
             Input:\n{}",
            serde_json::to_string_pretty(&input).expect("planner input is serializable")
        )
    }

    /// Performs the mandatory Rust-side validation before a decision is consumed.
    pub fn validate_decision(
        &self,
        decision: PlannerDecision,
    ) -> Result<ValidatedPlannerDecision, PlannerError> {
        if decision.reason.trim().is_empty() {
            return Err(PlannerError::InvalidDecision(
                "decision reason must not be empty".to_owned(),
            ));
        }
        if !matches!(decision.execution_intent, ExecutionIntent::Execute) {
            return Err(PlannerError::InvalidDecision(
                "only the execute intent is supported".to_owned(),
            ));
        }
        let Some(entry) = self
            .providers
            .iter()
            .find(|entry| entry.provider == decision.provider)
        else {
            return Err(PlannerError::UnknownProvider {
                provider: decision.provider,
            });
        };
        if !entry.available {
            return Err(PlannerError::ProviderUnavailable {
                provider: decision.provider,
            });
        }
        Ok(ValidatedPlannerDecision { decision })
    }
}

/// The action a planner wants the orchestrator to perform.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExecutionIntent {
    Execute,
}

/// Structured semantic output from a planner.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlannerDecision {
    provider: ProviderRef,
    reason: String,
    execution_intent: ExecutionIntent,
}

impl PlannerDecision {
    #[must_use]
    pub fn new(
        provider: ProviderRef,
        reason: impl Into<String>,
        execution_intent: ExecutionIntent,
    ) -> Self {
        Self {
            provider,
            reason: reason.into(),
            execution_intent,
        }
    }

    #[must_use]
    pub fn execute(provider: ProviderRef, reason: impl Into<String>) -> Self {
        Self::new(provider, reason, ExecutionIntent::Execute)
    }

    #[must_use]
    pub const fn provider(&self) -> &ProviderRef {
        &self.provider
    }

    #[must_use]
    pub fn reason(&self) -> &str {
        &self.reason
    }

    #[must_use]
    pub const fn execution_intent(&self) -> ExecutionIntent {
        self.execution_intent
    }

    /// Validates this decision against immutable Rust-owned facts.
    pub fn validate(
        self,
        request: &PlannerRequest,
    ) -> Result<ValidatedPlannerDecision, PlannerError> {
        request.validate_decision(self)
    }
}

/// A decision that has passed all Rust-side provider and intent checks.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ValidatedPlannerDecision {
    decision: PlannerDecision,
}

impl ValidatedPlannerDecision {
    #[must_use]
    pub const fn decision(&self) -> &PlannerDecision {
        &self.decision
    }

    #[must_use]
    pub const fn provider(&self) -> &ProviderRef {
        self.decision.provider()
    }

    #[must_use]
    pub fn reason(&self) -> &str {
        self.decision.reason()
    }

    #[must_use]
    pub const fn execution_intent(&self) -> ExecutionIntent {
        self.decision.execution_intent()
    }
}

/// Semantic planner backend.  Implementations must not mutate a Task.
pub trait Planner {
    fn plan(&self, request: &PlannerRequest) -> Result<PlannerDecision, PlannerError>;
}

/// Application boundary that makes it impossible to consume an unvalidated decision.
#[derive(Clone, Debug)]
pub struct PlannerService<P> {
    planner: P,
}

impl<P> PlannerService<P>
where
    P: Planner,
{
    #[must_use]
    pub fn new(planner: P) -> Self {
        Self { planner }
    }

    #[must_use]
    pub const fn planner(&self) -> &P {
        &self.planner
    }

    pub fn plan(
        &self,
        task: &Task,
        providers: impl IntoIterator<Item = ProviderAvailability>,
    ) -> Result<ValidatedPlannerDecision, PlannerError> {
        self.plan_request(&PlannerRequest::from_task(task, providers))
    }

    pub fn plan_request(
        &self,
        request: &PlannerRequest,
    ) -> Result<ValidatedPlannerDecision, PlannerError> {
        self.planner.plan(request)?.validate(request)
    }
}

/// Codex CLI adapter for the planner boundary.
#[derive(Clone, Debug)]
pub struct CodexPlanner {
    executable: OsString,
    command_prefix: Vec<OsString>,
    runner: ProcessRunner,
}

impl Default for CodexPlanner {
    fn default() -> Self {
        Self::new()
    }
}

impl CodexPlanner {
    #[must_use]
    pub fn new() -> Self {
        Self {
            executable: OsString::from(CODEX_COMMAND),
            command_prefix: Vec::new(),
            runner: ProcessRunner,
        }
    }

    #[must_use]
    pub fn with_executable(executable: impl Into<OsString>) -> Self {
        Self {
            executable: executable.into(),
            ..Self::new()
        }
    }

    /// Adds an executable prefix, primarily for deterministic fake CLI tests.
    #[must_use]
    pub fn with_command_prefix(
        mut self,
        command_prefix: impl IntoIterator<Item = impl Into<OsString>>,
    ) -> Self {
        self.command_prefix = command_prefix.into_iter().map(Into::into).collect();
        self
    }

    #[must_use]
    pub fn executable(&self) -> &OsStr {
        &self.executable
    }

    /// Returns the schema passed to `codex exec --output-schema`.
    #[must_use]
    pub fn output_schema() -> &'static str {
        r#"{"type":"object","additionalProperties":false,"required":["provider","reason","execution_intent"],"properties":{"provider":{"type":"string","minLength":1},"reason":{"type":"string","minLength":1},"execution_intent":{"type":"string","enum":["execute"]}}}"#
    }

    fn process_request(
        &self,
        request: &PlannerRequest,
        schema_path: &Path,
        output_path: &Path,
    ) -> ProcessRequest {
        ProcessRequest::new(self.executable.clone())
            .args(self.command_prefix.iter().cloned())
            .args([
                OsString::from("exec"),
                OsString::from("--sandbox"),
                OsString::from("read-only"),
                OsString::from("--ephemeral"),
                OsString::from("--output-schema"),
                schema_path.as_os_str().to_owned(),
                OsString::from("--output-last-message"),
                output_path.as_os_str().to_owned(),
            ])
            .arg(request.prompt())
            .timeout(request.timeout())
    }

    fn execute(&self, request: &PlannerRequest) -> Result<PlannerDecision, PlannerError> {
        let schema =
            TemporaryFile::create("ai-dev-orchestrator-planner-schema", Self::output_schema())?;
        let output = TemporaryPath::new("ai-dev-orchestrator-planner-output")?;
        let process = self
            .runner
            .run(self.process_request(request, schema.path(), output.path()))
            .map_err(|error| self.map_process_error(error, request.timeout()))?;

        let stdout = String::from_utf8_lossy(&process.stdout).into_owned();
        let text = if output.path().is_file() {
            fs::read_to_string(output.path()).map_err(|error| PlannerError::OutputReadFailed {
                message: error.to_string(),
            })?
        } else {
            stdout
        };
        parse_decision(&text)
    }

    fn map_process_error(&self, error: ProcessError, timeout: Duration) -> PlannerError {
        match error {
            ProcessError::Spawn(error) => PlannerError::CodexUnavailable {
                message: format!("failed to start Codex CLI: {error}"),
            },
            ProcessError::Io(error) => PlannerError::CodexExecutionFailed {
                message: format!("Codex process I/O failed: {error}"),
            },
            ProcessError::TimedOut(_) => PlannerError::CodexTimedOut { timeout },
            ProcessError::Cancelled(_) => PlannerError::CodexCancelled,
            ProcessError::NonZeroExit(output) => PlannerError::CodexExecutionFailed {
                message: diagnostic(&output.stdout, &output.stderr, output.exit_code()),
            },
        }
    }
}

impl Planner for CodexPlanner {
    fn plan(&self, request: &PlannerRequest) -> Result<PlannerDecision, PlannerError> {
        self.execute(request)
    }
}

/// Typed failures at the planner boundary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PlannerError {
    UnknownProvider { provider: ProviderRef },
    ProviderUnavailable { provider: ProviderRef },
    InvalidDecision(String),
    CodexUnavailable { message: String },
    CodexExecutionFailed { message: String },
    CodexTimedOut { timeout: Duration },
    CodexCancelled,
    OutputReadFailed { message: String },
    InvalidStructuredOutput { message: String },
}

impl std::fmt::Display for PlannerError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownProvider { provider } => {
                write!(
                    formatter,
                    "planner selected unknown provider: {}",
                    provider.as_str()
                )
            }
            Self::ProviderUnavailable { provider } => write!(
                formatter,
                "planner selected unavailable provider: {}",
                provider.as_str()
            ),
            Self::InvalidDecision(message) => {
                write!(formatter, "invalid planner decision: {message}")
            }
            Self::CodexUnavailable { message } => {
                write!(formatter, "Codex planner unavailable: {message}")
            }
            Self::CodexExecutionFailed { message } => {
                write!(formatter, "Codex planner execution failed: {message}")
            }
            Self::CodexTimedOut { timeout } => {
                write!(formatter, "Codex planner timed out after {timeout:?}")
            }
            Self::CodexCancelled => formatter.write_str("Codex planner was cancelled"),
            Self::OutputReadFailed { message } => {
                write!(formatter, "planner output could not be read: {message}")
            }
            Self::InvalidStructuredOutput { message } => {
                write!(formatter, "invalid planner structured output: {message}")
            }
        }
    }
}

impl std::error::Error for PlannerError {}

fn parse_decision(text: &str) -> Result<PlannerDecision, PlannerError> {
    let object = serde_json::from_str::<Value>(text.trim())
        .map_err(|error| PlannerError::InvalidStructuredOutput {
            message: error.to_string(),
        })?
        .as_object()
        .cloned()
        .ok_or_else(|| PlannerError::InvalidStructuredOutput {
            message: "planner output must be a JSON object".to_owned(),
        })?;
    if let Some(key) = object
        .keys()
        .find(|key| !matches!(key.as_str(), "provider" | "reason" | "execution_intent"))
    {
        return Err(PlannerError::InvalidStructuredOutput {
            message: format!("unknown field in planner output: {key}"),
        });
    }
    let provider = object
        .get("provider")
        .and_then(Value::as_str)
        .ok_or_else(|| PlannerError::InvalidStructuredOutput {
            message: "provider must be a string".to_owned(),
        })?
        .to_owned();
    let reason = object
        .get("reason")
        .and_then(Value::as_str)
        .ok_or_else(|| PlannerError::InvalidStructuredOutput {
            message: "reason must be a string".to_owned(),
        })?
        .to_owned();
    let intent = match object.get("execution_intent").ok_or_else(|| {
        PlannerError::InvalidStructuredOutput {
            message: "execution_intent is required".to_owned(),
        }
    })? {
        Value::String(value) if value == "execute" => ExecutionIntent::Execute,
        other => {
            return Err(PlannerError::InvalidStructuredOutput {
                message: format!("unsupported execution_intent: {other}"),
            });
        }
    };
    if provider.trim().is_empty() {
        return Err(PlannerError::InvalidStructuredOutput {
            message: "provider must not be empty".to_owned(),
        });
    }
    Ok(PlannerDecision::new(
        ProviderRef::new(provider),
        reason,
        intent,
    ))
}

fn diagnostic(stdout: &[u8], stderr: &[u8], status: Option<i32>) -> String {
    let stdout = String::from_utf8_lossy(stdout).trim().to_owned();
    let stderr = String::from_utf8_lossy(stderr).trim().to_owned();
    format!("exit status {status:?}; stdout: {stdout}; stderr: {stderr}")
}

#[derive(Debug)]
struct TemporaryFile {
    path: PathBuf,
}

impl TemporaryFile {
    fn create(prefix: &str, contents: &str) -> Result<Self, PlannerError> {
        let path = unique_temp_path(prefix)?;
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|error| PlannerError::OutputReadFailed {
                message: format!("temporary planner file could not be created: {error}"),
            })?;
        file.write_all(contents.as_bytes())
            .map_err(|error| PlannerError::OutputReadFailed {
                message: format!("temporary planner file could not be written: {error}"),
            })?;
        Ok(Self { path })
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TemporaryFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

#[derive(Debug)]
struct TemporaryPath {
    path: PathBuf,
}

impl TemporaryPath {
    fn new(prefix: &str) -> Result<Self, PlannerError> {
        Ok(Self {
            path: unique_temp_path(prefix)?,
        })
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TemporaryPath {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

fn unique_temp_path(prefix: &str) -> Result<PathBuf, PlannerError> {
    static NEXT_ID: AtomicU64 = AtomicU64::new(0);
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!("{prefix}-{}-{id}", std::process::id()));
    if path.exists() {
        return Err(PlannerError::OutputReadFailed {
            message: format!("temporary planner path already exists: {}", path.display()),
        });
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn task() -> Task {
        Task::new(
            TaskId::new("task-1"),
            "implement the change",
            TaskRole::new("developer"),
        )
    }

    #[test]
    fn validates_unknown_and_unavailable_provider_before_consumption() {
        let request = PlannerRequest::from_task(
            &task(),
            [
                ProviderAvailability::available(ProviderRef::new("codex")),
                ProviderAvailability::unavailable(ProviderRef::new("copilot")),
            ],
        );
        assert!(matches!(
            PlannerDecision::execute(ProviderRef::new("missing"), "reason").validate(&request),
            Err(PlannerError::UnknownProvider { .. })
        ));
        assert!(matches!(
            PlannerDecision::execute(ProviderRef::new("copilot"), "reason").validate(&request),
            Err(PlannerError::ProviderUnavailable { .. })
        ));
    }

    #[derive(Clone)]
    struct FakePlanner {
        decision: Result<PlannerDecision, PlannerError>,
    }

    impl Planner for FakePlanner {
        fn plan(&self, _: &PlannerRequest) -> Result<PlannerDecision, PlannerError> {
            self.decision.clone()
        }
    }

    #[test]
    fn fake_planner_is_deterministic_and_service_validates_its_output() {
        let service = PlannerService::new(FakePlanner {
            decision: Ok(PlannerDecision::execute(
                ProviderRef::new("codex"),
                "best fit for the task",
            )),
        });
        let request = PlannerRequest::from_task(
            &task(),
            [ProviderAvailability::available(ProviderRef::new("codex"))],
        );
        let first = service.plan_request(&request).unwrap();
        let second = service.plan_request(&request).unwrap();
        assert_eq!(first, second);
        assert_eq!(first.provider().as_str(), "codex");
        assert_eq!(first.execution_intent(), ExecutionIntent::Execute);
    }

    #[cfg(unix)]
    #[test]
    fn codex_fake_receives_schema_and_read_only_sandbox_and_parses_output() {
        let script = r#"
for arg in "$@"; do
  case "$arg" in
    --sandbox) seen_sandbox=1;;
    read-only) seen_read_only=1;;
    --output-schema) seen_schema=1;;
  esac
done
printf '{"provider":"codex","reason":"deterministic fake","execution_intent":"execute"}'
if [ "$seen_sandbox" = 1 ] && [ "$seen_read_only" = 1 ] && [ "$seen_schema" = 1 ]; then exit 0; fi
exit 9
"#;
        let planner =
            CodexPlanner::with_executable("sh").with_command_prefix(["-c", script, "fake-codex"]);
        let request = PlannerRequest::from_task(
            &task(),
            [ProviderAvailability::available(ProviderRef::new("codex"))],
        );
        let decision = planner.plan(&request).unwrap();
        assert_eq!(decision.provider().as_str(), "codex");
        assert!(!PathBuf::from(CodexPlanner::output_schema()).exists());
    }

    #[test]
    fn planner_prompt_contains_rust_availability_facts() {
        let request = PlannerRequest::from_task(
            &task(),
            [ProviderAvailability::unavailable(ProviderRef::new(
                "copilot",
            ))],
        );
        let prompt = request.prompt();
        assert!(prompt.contains("\"available\": false"));
        assert!(prompt.contains("implement the change"));
    }
}
