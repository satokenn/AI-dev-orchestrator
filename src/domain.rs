use std::fmt;

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct TaskId(String);

impl TaskId {
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self { Self(value.into()) }
    #[must_use]
    pub fn as_str(&self) -> &str { &self.0 }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct AttemptId(String);

impl AttemptId {
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self { Self(value.into()) }
    #[must_use]
    pub fn as_str(&self) -> &str { &self.0 }
}

/// A semantic role requested for a task. The value remains extensible in v0.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct TaskRole(String);

impl TaskRole {
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self { Self(value.into()) }
    #[must_use]
    pub fn as_str(&self) -> &str { &self.0 }
}

/// Reference to the provider selected for an attempt.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct ProviderRef(String);

impl ProviderRef {
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self { Self(value.into()) }
    #[must_use]
    pub fn as_str(&self) -> &str { &self.0 }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TaskState { Pending, Active, Completed, Failed, Cancelled }

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AttemptState { Queued, Running, Validating, Succeeded, Failed, Cancelled }

/// A report returned by an agent. It is not a success decision.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AgentResult { summary: String, reported_success: bool }

impl AgentResult {
    #[must_use]
    pub fn new(summary: impl Into<String>, reported_success: bool) -> Self {
        Self { summary: summary.into(), reported_success }
    }
    #[must_use]
    pub fn summary(&self) -> &str { &self.summary }
    #[must_use]
    pub const fn reported_success(&self) -> bool { self.reported_success }
}

/// A mechanical validation result produced by a validator.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ValidationResult { summary: String, passed: bool }

impl ValidationResult {
    #[must_use]
    pub fn new(summary: impl Into<String>, passed: bool) -> Self {
        Self { summary: summary.into(), passed }
    }
    #[must_use]
    pub fn summary(&self) -> &str { &self.summary }
    #[must_use]
    pub const fn passed(&self) -> bool { self.passed }
}

/// One provider-reported usage or cost metric.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UsageMetric { name: String, value: String, unit: String }

impl UsageMetric {
    #[must_use]
    pub fn new(name: impl Into<String>, value: impl Into<String>, unit: impl Into<String>) -> Self {
        Self { name: name.into(), value: value.into(), unit: unit.into() }
    }
    #[must_use]
    pub fn name(&self) -> &str { &self.name }
    #[must_use]
    pub fn value(&self) -> &str { &self.value }
    #[must_use]
    pub fn unit(&self) -> &str { &self.unit }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct UsageCost { metrics: Vec<UsageMetric> }

impl UsageCost {
    #[must_use]
    pub fn new(metrics: impl IntoIterator<Item = UsageMetric>) -> Self {
        Self { metrics: metrics.into_iter().collect() }
    }
    #[must_use]
    pub fn metrics(&self) -> &[UsageMetric] { &self.metrics }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DomainError {
    InvalidTaskTransition { from: TaskState, to: TaskState },
    InvalidAttemptTransition { from: AttemptState, to: AttemptState },
    TaskClosed { state: TaskState },
}

impl fmt::Display for DomainError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidTaskTransition { from, to } => write!(formatter, "invalid task transition: {from:?} -> {to:?}"),
            Self::InvalidAttemptTransition { from, to } => write!(formatter, "invalid attempt transition: {from:?} -> {to:?}"),
            Self::TaskClosed { state } => write!(formatter, "task is closed in state {state:?}"),
        }
    }
}

impl std::error::Error for DomainError {}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Attempt {
    id: AttemptId,
    provider: ProviderRef,
    state: AttemptState,
    agent_result: Option<AgentResult>,
    validation_results: Vec<ValidationResult>,
    usage_cost: Option<UsageCost>,
}

impl Attempt {
    #[must_use]
    pub fn new(id: AttemptId, provider: ProviderRef) -> Self {
        Self { id, provider, state: AttemptState::Queued, agent_result: None, validation_results: Vec::new(), usage_cost: None }
    }
    #[must_use]
    pub const fn id(&self) -> &AttemptId { &self.id }
    #[must_use]
    pub const fn provider(&self) -> &ProviderRef { &self.provider }
    #[must_use]
    pub const fn state(&self) -> AttemptState { self.state }
    #[must_use]
    pub const fn agent_result(&self) -> Option<&AgentResult> { self.agent_result.as_ref() }
    #[must_use]
    pub fn validation_results(&self) -> &[ValidationResult] { &self.validation_results }
    #[must_use]
    pub const fn usage_cost(&self) -> Option<&UsageCost> { self.usage_cost.as_ref() }

    pub fn start(&mut self) -> Result<(), DomainError> { self.transition(AttemptState::Running) }
    pub fn finish(&mut self) -> Result<(), DomainError> { self.transition(AttemptState::Validating) }

    /// Records an agent report without changing the attempt state.
    pub fn record_agent_result(&mut self, result: AgentResult) -> Result<(), DomainError> {
        if matches!(self.state, AttemptState::Succeeded | AttemptState::Failed | AttemptState::Cancelled) {
            return Err(DomainError::InvalidAttemptTransition { from: self.state, to: self.state });
        }
        self.agent_result = Some(result);
        Ok(())
    }

    /// Applies a validator result and makes the attempt's terminal state explicit.
    pub fn apply_validation(&mut self, result: ValidationResult) -> Result<(), DomainError> {
        let next = if result.passed() { AttemptState::Succeeded } else { AttemptState::Failed };
        if self.state != AttemptState::Validating {
            return Err(DomainError::InvalidAttemptTransition { from: self.state, to: next });
        }
        self.validation_results.push(result);
        self.transition(next)
    }

    pub fn fail(&mut self) -> Result<(), DomainError> { self.transition(AttemptState::Failed) }
    pub fn cancel(&mut self) -> Result<(), DomainError> { self.transition(AttemptState::Cancelled) }
    pub fn set_usage_cost(&mut self, usage_cost: UsageCost) { self.usage_cost = Some(usage_cost); }

    fn transition(&mut self, to: AttemptState) -> Result<(), DomainError> {
        if matches!((self.state, to),
            (AttemptState::Queued, AttemptState::Running | AttemptState::Cancelled)
                | (AttemptState::Running, AttemptState::Validating | AttemptState::Failed | AttemptState::Cancelled)
                | (AttemptState::Validating, AttemptState::Succeeded | AttemptState::Failed | AttemptState::Cancelled)) {
            self.state = to;
            Ok(())
        } else {
            Err(DomainError::InvalidAttemptTransition { from: self.state, to })
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Task {
    id: TaskId,
    description: String,
    role: TaskRole,
    state: TaskState,
    attempts: Vec<Attempt>,
}

impl Task {
    #[must_use]
    pub fn new(id: TaskId, description: impl Into<String>, role: TaskRole) -> Self {
        Self { id, description: description.into(), role, state: TaskState::Pending, attempts: Vec::new() }
    }
    #[must_use]
    pub const fn id(&self) -> &TaskId { &self.id }
    #[must_use]
    pub fn description(&self) -> &str { &self.description }
    #[must_use]
    pub const fn role(&self) -> &TaskRole { &self.role }
    #[must_use]
    pub const fn state(&self) -> TaskState { self.state }
    #[must_use]
    pub fn attempts(&self) -> &[Attempt] { &self.attempts }

    pub fn start(&mut self) -> Result<(), DomainError> { self.transition(TaskState::Active) }
    pub fn complete(&mut self) -> Result<(), DomainError> { self.transition(TaskState::Completed) }
    pub fn fail(&mut self) -> Result<(), DomainError> { self.transition(TaskState::Failed) }

    pub fn cancel(&mut self) -> Result<(), DomainError> {
        self.transition(TaskState::Cancelled)?;
        for attempt in &mut self.attempts {
            if matches!(attempt.state(), AttemptState::Queued | AttemptState::Running | AttemptState::Validating) {
                attempt.cancel()?;
            }
        }
        Ok(())
    }

    pub fn add_attempt(&mut self, attempt: Attempt) -> Result<(), DomainError> {
        if matches!(self.state, TaskState::Completed | TaskState::Failed | TaskState::Cancelled) {
            return Err(DomainError::TaskClosed { state: self.state });
        }
        self.attempts.push(attempt);
        Ok(())
    }

    fn transition(&mut self, to: TaskState) -> Result<(), DomainError> {
        if matches!((self.state, to),
            (TaskState::Pending, TaskState::Active | TaskState::Cancelled)
                | (TaskState::Active, TaskState::Completed | TaskState::Failed | TaskState::Cancelled)) {
            self.state = to;
            Ok(())
        } else {
            Err(DomainError::InvalidTaskTransition { from: self.state, to })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task() -> Task {
        Task::new(TaskId::new("task-1"), "Implement the adapter", TaskRole::new("developer"))
    }
    fn attempt(id: &str) -> Attempt { Attempt::new(AttemptId::new(id), ProviderRef::new("codex")) }

    #[test]
    fn task_keeps_multiple_attempts_with_provider_on_attempt() {
        let mut task = task();
        task.add_attempt(attempt("attempt-1")).unwrap();
        task.add_attempt(Attempt::new(AttemptId::new("attempt-2"), ProviderRef::new("github-copilot-cli"))).unwrap();
        assert_eq!(task.attempts().len(), 2);
        assert_eq!(task.role().as_str(), "developer");
        assert_eq!(task.attempts()[1].provider().as_str(), "github-copilot-cli");
    }

    #[test]
    fn task_state_allows_only_declared_transitions() {
        let mut task = task();
        task.start().unwrap();
        task.complete().unwrap();
        assert_eq!(task.start(), Err(DomainError::InvalidTaskTransition { from: TaskState::Completed, to: TaskState::Active }));
    }

    #[test]
    fn attempt_state_allows_successful_validation_flow() {
        let mut attempt = attempt("attempt-1");
        attempt.start().unwrap();
        attempt.finish().unwrap();
        attempt.apply_validation(ValidationResult::new("cargo test passed", true)).unwrap();
        assert_eq!(attempt.state(), AttemptState::Succeeded);
        assert_eq!(attempt.validation_results().len(), 1);
    }

    #[test]
    fn invalid_attempt_transition_is_rejected() {
        let mut attempt = attempt("attempt-1");
        assert_eq!(attempt.finish(), Err(DomainError::InvalidAttemptTransition { from: AttemptState::Queued, to: AttemptState::Validating }));
        attempt.start().unwrap();
        attempt.finish().unwrap();
        attempt.apply_validation(ValidationResult::new("cargo test failed", false)).unwrap();
        assert_eq!(attempt.cancel(), Err(DomainError::InvalidAttemptTransition { from: AttemptState::Failed, to: AttemptState::Cancelled }));
    }

    #[test]
    fn agent_result_does_not_change_state_or_succeed_attempt() {
        let mut attempt = attempt("attempt-1");
        attempt.start().unwrap();
        attempt.record_agent_result(AgentResult::new("implementation complete", true)).unwrap();
        assert_eq!(attempt.state(), AttemptState::Running);
        assert!(attempt.agent_result().unwrap().reported_success());
    }

    #[test]
    fn validation_failure_keeps_task_active_for_next_attempt() {
        let mut task = task();
        task.start().unwrap();
        let mut first = attempt("attempt-1");
        first.start().unwrap();
        first.finish().unwrap();
        first.apply_validation(ValidationResult::new("cargo test failed", false)).unwrap();
        task.add_attempt(first).unwrap();
        assert_eq!(task.state(), TaskState::Active);
        task.add_attempt(attempt("attempt-2")).unwrap();
        assert_eq!(task.attempts().len(), 2);
    }

    #[test]
    fn cancelling_task_cancels_non_terminal_attempts_and_closes_task() {
        let mut task = task();
        task.start().unwrap();
        let mut running = attempt("attempt-1");
        running.start().unwrap();
        task.add_attempt(running).unwrap();
        let mut failed = attempt("attempt-2");
        failed.start().unwrap();
        failed.fail().unwrap();
        task.add_attempt(failed).unwrap();
        task.cancel().unwrap();
        assert_eq!(task.state(), TaskState::Cancelled);
        assert_eq!(task.attempts()[0].state(), AttemptState::Cancelled);
        assert_eq!(task.attempts()[1].state(), AttemptState::Failed);
        assert_eq!(task.add_attempt(attempt("attempt-3")), Err(DomainError::TaskClosed { state: TaskState::Cancelled }));
    }

    #[test]
    fn usage_cost_keeps_provider_metrics_with_the_attempt() {
        let mut attempt = attempt("attempt-1");
        attempt.set_usage_cost(UsageCost::new([
            UsageMetric::new("input_tokens", "1000", "tokens"),
            UsageMetric::new("cost", "0.25", "USD"),
        ]));
        let metrics = attempt.usage_cost().unwrap().metrics();
        assert_eq!(metrics[0].name(), "input_tokens");
        assert_eq!(metrics[1].unit(), "USD");
    }
}
