//! Deterministic, process-backed workspace validation.

use std::ffi::OsString;
use std::fmt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::{
    CancellationToken, ProcessError, ProcessOutput, ProcessRequest, ProcessRunner,
    ValidationCheckResult, ValidationResult,
};

/// One mechanical check to run in a workspace.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ValidationCheck {
    name: String,
    command: OsString,
    args: Vec<OsString>,
    cwd: Option<PathBuf>,
    timeout: Option<Duration>,
}

impl ValidationCheck {
    /// Creates a check. Its working directory defaults to the validation workspace.
    #[must_use]
    pub fn new(name: impl Into<String>, command: impl Into<OsString>) -> Self {
        Self {
            name: name.into(),
            command: command.into(),
            args: Vec::new(),
            cwd: None,
            timeout: None,
        }
    }

    #[must_use]
    pub fn arg(mut self, arg: impl Into<OsString>) -> Self {
        self.args.push(arg.into());
        self
    }

    #[must_use]
    pub fn args(mut self, args: impl IntoIterator<Item = impl Into<OsString>>) -> Self {
        self.args.extend(args.into_iter().map(Into::into));
        self
    }

    /// Overrides the workspace as the check's cwd. Leave unset for the normal workspace.
    #[must_use]
    pub fn cwd(mut self, cwd: impl Into<PathBuf>) -> Self {
        self.cwd = Some(cwd.into());
        self
    }

    /// Sets a per-check timeout. Zero is rejected by `CommandValidator::validate`.
    #[must_use]
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }
    #[must_use]
    pub fn command(&self) -> &OsString {
        &self.command
    }
    #[must_use]
    pub fn args_ref(&self) -> &[OsString] {
        &self.args
    }
    #[must_use]
    pub fn cwd_ref(&self) -> Option<&Path> {
        self.cwd.as_deref()
    }
    #[must_use]
    pub const fn timeout_value(&self) -> Option<Duration> {
        self.timeout
    }

    fn request_with_cwd(&self, cwd: PathBuf) -> ProcessRequest {
        let request = ProcessRequest::new(self.command.clone())
            .args(self.args.clone())
            .cwd(cwd);
        match self.timeout {
            Some(timeout) => request.timeout(timeout),
            None => request,
        }
    }
}

/// Errors which prevent validation from starting.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ValidatorError {
    InvalidWorkspace {
        workspace: PathBuf,
        reason: String,
    },
    WorkspaceOutsideBoundary {
        workspace: PathBuf,
        cwd: PathBuf,
        reason: String,
    },
    NoChecksConfigured,
    InvalidCheck {
        name: String,
        reason: String,
    },
    Cancelled,
    Interrupted {
        reason: crate::StopReason,
        stopped: bool,
    },
}

impl fmt::Display for ValidatorError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidWorkspace { workspace, reason } => write!(
                formatter,
                "invalid validation workspace '{}': {reason}",
                workspace.display()
            ),
            Self::WorkspaceOutsideBoundary {
                workspace,
                cwd,
                reason,
            } => write!(
                formatter,
                "validation cwd '{}' is outside workspace '{}': {reason}",
                cwd.display(),
                workspace.display()
            ),
            Self::NoChecksConfigured => formatter.write_str("no validation checks configured"),
            Self::InvalidCheck { name, reason } => {
                write!(formatter, "invalid validation check '{name}': {reason}")
            }
            Self::Cancelled => formatter.write_str("validation was cancelled"),
            Self::Interrupted { reason, stopped } => write!(
                formatter,
                "validation was interrupted ({reason:?}, stopped={stopped})"
            ),
        }
    }
}

impl std::error::Error for ValidatorError {}

/// Common API for deterministic validators.
pub trait Validator {
    fn validate(&self, workspace: &Path) -> Result<ValidationResult, ValidatorError>;

    /// Runs validation while observing a caller-owned cancellation signal.
    ///
    /// Existing validators remain source-compatible; validators that launch
    /// cancellable work should override this method.
    fn validate_with_cancellation(
        &self,
        workspace: &Path,
        _cancellation: CancellationToken,
    ) -> Result<ValidationResult, ValidatorError> {
        self.validate(workspace)
    }
}

/// Runs a sequence of configured process checks in order.
#[derive(Clone, Debug)]
pub struct CommandValidator {
    checks: Vec<ValidationCheck>,
    runner: ProcessRunner,
}

impl CommandValidator {
    #[must_use]
    pub fn new(checks: impl IntoIterator<Item = ValidationCheck>) -> Self {
        Self {
            checks: checks.into_iter().collect(),
            runner: ProcessRunner,
        }
    }

    #[must_use]
    pub fn checks(&self) -> &[ValidationCheck] {
        &self.checks
    }

    fn validate_workspace(workspace: &Path) -> Result<(), ValidatorError> {
        if workspace.as_os_str().is_empty() {
            return Err(ValidatorError::InvalidWorkspace {
                workspace: workspace.to_owned(),
                reason: "path must not be empty".to_owned(),
            });
        }
        let metadata =
            std::fs::metadata(workspace).map_err(|error| ValidatorError::InvalidWorkspace {
                workspace: workspace.to_owned(),
                reason: error.to_string(),
            })?;
        if !metadata.is_dir() {
            return Err(ValidatorError::InvalidWorkspace {
                workspace: workspace.to_owned(),
                reason: "path is not a directory".to_owned(),
            });
        }
        Ok(())
    }

    fn resolve_check_cwd(
        workspace: &Path,
        check: &ValidationCheck,
    ) -> Result<PathBuf, ValidatorError> {
        let Some(cwd) = check.cwd_ref() else {
            return Ok(workspace.to_owned());
        };
        if cwd
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
        {
            return Err(ValidatorError::WorkspaceOutsideBoundary {
                workspace: workspace.to_owned(),
                cwd: cwd.to_owned(),
                reason: "cwd must not contain '..'".to_owned(),
            });
        }
        let candidate = if cwd.is_absolute() {
            cwd.to_owned()
        } else {
            workspace.join(cwd)
        };
        let canonical_workspace =
            std::fs::canonicalize(workspace).map_err(|error| ValidatorError::InvalidWorkspace {
                workspace: workspace.to_owned(),
                reason: error.to_string(),
            })?;
        let canonical_cwd = std::fs::canonicalize(&candidate).map_err(|error| {
            ValidatorError::WorkspaceOutsideBoundary {
                workspace: workspace.to_owned(),
                cwd: cwd.to_owned(),
                reason: error.to_string(),
            }
        })?;
        if !canonical_cwd.starts_with(&canonical_workspace) {
            return Err(ValidatorError::WorkspaceOutsideBoundary {
                workspace: workspace.to_owned(),
                cwd: cwd.to_owned(),
                reason: "cwd resolves outside the workspace".to_owned(),
            });
        }
        Ok(canonical_cwd)
    }
}

impl Validator for CommandValidator {
    fn validate(&self, workspace: &Path) -> Result<ValidationResult, ValidatorError> {
        self.validate_with_cancellation(workspace, CancellationToken::new())
    }

    fn validate_with_cancellation(
        &self,
        workspace: &Path,
        cancellation: CancellationToken,
    ) -> Result<ValidationResult, ValidatorError> {
        CommandValidator::validate_with_cancellation(self, workspace, cancellation)
    }
}

impl CommandValidator {
    /// Runs configured checks in order, passing one cancellation signal to every process.
    pub fn validate_with_cancellation(
        &self,
        workspace: &Path,
        token: CancellationToken,
    ) -> Result<ValidationResult, ValidatorError> {
        if self.checks.is_empty() {
            return Err(ValidatorError::NoChecksConfigured);
        }
        for check in &self.checks {
            if check.name.trim().is_empty() || check.command.is_empty() {
                return Err(ValidatorError::InvalidCheck {
                    name: check.name.clone(),
                    reason: "name and command must not be empty".to_owned(),
                });
            }
            if check.timeout.is_some_and(|timeout| timeout.is_zero()) {
                return Err(ValidatorError::InvalidCheck {
                    name: check.name.clone(),
                    reason: "timeout must be greater than zero".to_owned(),
                });
            }
        }
        Self::validate_workspace(workspace)?;
        let resolved_cwds: Vec<_> = self
            .checks
            .iter()
            .map(|check| Self::resolve_check_cwd(workspace, check))
            .collect::<Result<_, _>>()?;
        let checks: Vec<_> = self
            .checks
            .iter()
            .zip(resolved_cwds)
            .map(|(check, cwd)| {
                let request = check.request_with_cwd(cwd);
                match self.runner.run_with_cancellation(request, token.clone()) {
                    Ok(output) => Ok(result_from_output(check, output, true)),
                    Err(error) => result_from_error(check, error),
                }
            })
            .collect::<Result<_, _>>()?;
        Ok(ValidationResult::from_checks(
            "workspace validation",
            checks,
        ))
    }
}

/// The repository's standard Rust mechanical checks.
#[derive(Clone, Debug)]
pub struct RustValidator {
    inner: CommandValidator,
}

impl RustValidator {
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: CommandValidator::new(default_rust_checks()),
        }
    }

    #[must_use]
    pub fn checks(&self) -> &[ValidationCheck] {
        self.inner.checks()
    }
}

impl Default for RustValidator {
    fn default() -> Self {
        Self::new()
    }
}

impl Validator for RustValidator {
    fn validate(&self, workspace: &Path) -> Result<ValidationResult, ValidatorError> {
        self.inner.validate(workspace)
    }

    fn validate_with_cancellation(
        &self,
        workspace: &Path,
        cancellation: CancellationToken,
    ) -> Result<ValidationResult, ValidatorError> {
        self.inner
            .validate_with_cancellation(workspace, cancellation)
    }
}

/// Returns the standard checks in their required execution order.
#[must_use]
pub fn default_rust_checks() -> Vec<ValidationCheck> {
    vec![
        ValidationCheck::new("cargo fmt", "cargo").args(["fmt", "--all", "--", "--check"]),
        ValidationCheck::new("cargo clippy", "cargo").args([
            "clippy",
            "--workspace",
            "--all-targets",
            "--all-features",
            "--",
            "-D",
            "warnings",
        ]),
        ValidationCheck::new("cargo test", "cargo").args(["test", "--workspace", "--all-features"]),
    ]
}

fn result_from_output(
    check: &ValidationCheck,
    output: ProcessOutput,
    passed: bool,
) -> ValidationCheckResult {
    ValidationCheckResult::new(
        check.name(),
        passed,
        output.exit_code(),
        if passed {
            "process exited successfully"
        } else {
            "process exited with a non-zero status"
        },
    )
}

fn result_from_error(
    check: &ValidationCheck,
    error: ProcessError,
) -> Result<ValidationCheckResult, ValidatorError> {
    match error {
        ProcessError::NonZeroExit(output) => Ok(result_from_output(check, output, false)),
        ProcessError::TimedOut(output) => Ok(ValidationCheckResult::new(
            check.name(),
            false,
            output.exit_code(),
            "process timed out",
        )),
        ProcessError::Cancelled(_) | ProcessError::CancelledBeforeStart => {
            Err(ValidatorError::Cancelled)
        }
        ProcessError::Interrupted {
            reason, stopped, ..
        } => Err(ValidatorError::Interrupted { reason, stopped }),
        ProcessError::Spawn(error) | ProcessError::Io(error) | ProcessError::Stdin(error) => {
            let kind = error.kind();
            Ok(ValidationCheckResult::new(
                check.name(),
                false,
                None,
                format!("process failed ({kind:?})"),
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_rust_checks_are_exact_and_ordered() {
        let checks = default_rust_checks();
        assert_eq!(checks.len(), 3);
        assert_eq!(checks[0].command(), "cargo");
        assert_eq!(checks[0].args_ref(), ["fmt", "--all", "--", "--check"]);
        assert_eq!(
            checks[1].args_ref(),
            [
                "clippy",
                "--workspace",
                "--all-targets",
                "--all-features",
                "--",
                "-D",
                "warnings"
            ]
        );
        assert_eq!(
            checks[2].args_ref(),
            ["test", "--workspace", "--all-features"]
        );
    }
}
