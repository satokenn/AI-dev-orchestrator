//! Typed current observations for Provider CLI launchability.
//!
//! This module deliberately does not infer account authentication, model access,
//! quota, billing, or API usage from a local CLI version command. Those facts
//! remain unknown until a Provider exposes an authoritative source.

use std::{
    ffi::OsStr,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use crate::{
    AttemptRecord, ModelChoice, ModelRef, ProcessError, ProcessRequest, ProcessRunner, ProviderRef,
};

const CLI_PROBE_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Evidence<T> {
    Known {
        value: T,
        basis: EvidenceBasis,
        assessed_at_ms: i64,
        source: EvidenceSource,
    },
    Unknown {
        reason: String,
        assessed_at_ms: i64,
        source: EvidenceSource,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EvidenceBasis {
    Measured,
    Configured,
    Computed,
    Estimated,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EvidenceSourceKind {
    ProviderApi,
    ProviderCli,
    ExecutionLedger,
    RepositoryConfig,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EvidenceSource {
    pub kind: EvidenceSourceKind,
    pub reference: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AvailabilityStatus {
    Available,
    Unavailable { reason: String },
    Unknown { reason: String },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AvailabilityObservation {
    pub status: AvailabilityStatus,
    pub observed_at_ms: i64,
    pub source: EvidenceSource,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModelAvailabilityObservation {
    pub model: ModelChoice,
    pub availability: AvailabilityObservation,
}

/// Facts from one bounded local CLI probe.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProviderObservation {
    pub provider: ProviderRef,
    /// Whether the configured executable could be started. This is not auth.
    pub cli_present: Evidence<bool>,
    /// Whether `--version` returned success. This is not model access.
    pub cli_version_check: Evidence<bool>,
    pub authentication: AvailabilityObservation,
    pub availability: AvailabilityObservation,
    /// The provider default is explicit, but its current model access is unknown.
    pub models: Vec<ModelAvailabilityObservation>,
}

/// Requested and observed target values read from one persisted Attempt.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AttemptTargetObservation {
    pub requested_provider: Evidence<ProviderRef>,
    pub requested_model: Evidence<ModelChoice>,
    pub observed_provider: Evidence<ProviderRef>,
    pub observed_model: Evidence<ModelRef>,
}

impl AttemptTargetObservation {
    #[must_use]
    pub fn from_ledger_record(record: &AttemptRecord, assessed_at_ms: i64) -> Self {
        let source = EvidenceSource {
            kind: EvidenceSourceKind::ExecutionLedger,
            reference: format!("attempt:{}", record.attempt().id().as_str()),
        };
        let attempt = record.attempt();
        Self {
            requested_provider: known_with_basis(
                attempt.provider().clone(),
                EvidenceBasis::Configured,
                assessed_at_ms,
                source.clone(),
            ),
            requested_model: optional_known(
                attempt.requested_model().cloned(),
                EvidenceBasis::Configured,
                "the Attempt does not record a requested model",
                assessed_at_ms,
                source.clone(),
            ),
            observed_provider: optional_known(
                attempt.observed_provider().cloned(),
                EvidenceBasis::Measured,
                "the Provider result did not record an observed Provider",
                assessed_at_ms,
                source.clone(),
            ),
            observed_model: optional_known(
                attempt.observed_model().cloned(),
                EvidenceBasis::Measured,
                "the Provider result did not record an observed Model",
                assessed_at_ms,
                source,
            ),
        }
    }
}

impl ProviderObservation {
    /// Probes an executable without retaining stdout/stderr, which may contain
    /// environment-specific or sensitive diagnostic text.
    #[must_use]
    pub fn probe_cli_at(
        provider: ProviderRef,
        executable: &OsStr,
        observed_at_ms: i64,
        runner: &ProcessRunner,
    ) -> Self {
        let source = EvidenceSource {
            kind: EvidenceSourceKind::ProviderCli,
            reference: format!("{} --version", provider.as_str()),
        };
        let result = runner.run(
            ProcessRequest::new(executable.to_os_string())
                .arg("--version")
                .timeout(CLI_PROBE_TIMEOUT),
        );

        let (cli_present, cli_version_check) = match result {
            Ok(output) => (
                known(true, observed_at_ms, source.clone()),
                known(output.status.success(), observed_at_ms, source.clone()),
            ),
            Err(ProcessError::Spawn(error)) if error.kind() == std::io::ErrorKind::NotFound => (
                known(false, observed_at_ms, source.clone()),
                unknown(
                    "the configured CLI executable was not found",
                    observed_at_ms,
                    source.clone(),
                ),
            ),
            Err(ProcessError::Spawn(_)) => (
                unknown(
                    "the configured CLI executable could not be started",
                    observed_at_ms,
                    source.clone(),
                ),
                unknown(
                    "the CLI version check did not complete",
                    observed_at_ms,
                    source.clone(),
                ),
            ),
            Err(ProcessError::NonZeroExit(_)) => (
                known(true, observed_at_ms, source.clone()),
                known(false, observed_at_ms, source.clone()),
            ),
            Err(ProcessError::TimedOut(_)) => (
                known(true, observed_at_ms, source.clone()),
                unknown(
                    "the CLI version check timed out",
                    observed_at_ms,
                    source.clone(),
                ),
            ),
            Err(ProcessError::Io(_)) => (
                known(true, observed_at_ms, source.clone()),
                unknown(
                    "the CLI version check did not complete",
                    observed_at_ms,
                    source.clone(),
                ),
            ),
            Err(ProcessError::Cancelled(_) | ProcessError::Interrupted { .. }) => (
                known(true, observed_at_ms, source.clone()),
                unknown(
                    "the CLI version check did not complete",
                    observed_at_ms,
                    source.clone(),
                ),
            ),
            Err(ProcessError::CancelledBeforeStart) => (
                unknown(
                    "the CLI probe did not complete",
                    observed_at_ms,
                    source.clone(),
                ),
                unknown(
                    "the CLI version check did not complete",
                    observed_at_ms,
                    source.clone(),
                ),
            ),
        };

        let cli_is_missing = matches!(cli_present, Evidence::Known { value: false, .. });
        let auth_reason = "the local CLI version probe does not verify account authentication";
        let provider_status = if cli_is_missing {
            AvailabilityStatus::Unavailable {
                reason: "the configured Provider CLI is not installed or not on PATH".to_owned(),
            }
        } else {
            AvailabilityStatus::Unknown {
                reason:
                    "CLI launchability does not establish authentication or current model access"
                        .to_owned(),
            }
        };
        let model_reason = "no authoritative current model-access probe is available";

        Self {
            provider,
            cli_present,
            cli_version_check,
            authentication: AvailabilityObservation {
                status: AvailabilityStatus::Unknown {
                    reason: auth_reason.to_owned(),
                },
                observed_at_ms,
                source: source.clone(),
            },
            availability: AvailabilityObservation {
                status: provider_status,
                observed_at_ms,
                source: source.clone(),
            },
            models: vec![ModelAvailabilityObservation {
                model: ModelChoice::ProviderDefault,
                availability: AvailabilityObservation {
                    status: AvailabilityStatus::Unknown {
                        reason: model_reason.to_owned(),
                    },
                    observed_at_ms,
                    source,
                },
            }],
        }
    }

    #[must_use]
    pub fn probe_cli(provider: ProviderRef, executable: &OsStr, runner: &ProcessRunner) -> Self {
        Self::probe_cli_at(provider, executable, current_time_ms(), runner)
    }
}

fn known<T>(value: T, assessed_at_ms: i64, source: EvidenceSource) -> Evidence<T> {
    Evidence::Known {
        value,
        basis: EvidenceBasis::Measured,
        assessed_at_ms,
        source,
    }
}

fn optional_known<T>(
    value: Option<T>,
    basis: EvidenceBasis,
    unknown_reason: &str,
    assessed_at_ms: i64,
    source: EvidenceSource,
) -> Evidence<T> {
    match value {
        Some(value) => known_with_basis(value, basis, assessed_at_ms, source),
        None => unknown(unknown_reason, assessed_at_ms, source),
    }
}

fn known_with_basis<T>(
    value: T,
    basis: EvidenceBasis,
    assessed_at_ms: i64,
    source: EvidenceSource,
) -> Evidence<T> {
    Evidence::Known {
        value,
        basis,
        assessed_at_ms,
        source,
    }
}

fn unknown<T>(reason: &str, assessed_at_ms: i64, source: EvidenceSource) -> Evidence<T> {
    Evidence::Unknown {
        reason: reason.to_owned(),
        assessed_at_ms,
        source,
    }
}

fn current_time_ms() -> i64 {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(duration) => duration.as_millis().min(i64::MAX as u128) as i64,
        Err(error) => -(error.duration().as_millis().min(i64::MAX as u128) as i64),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Attempt, AttemptId, ModelChoice, TaskId};
    use std::path::PathBuf;

    #[test]
    fn missing_executable_is_unavailable_without_claiming_auth_or_model_state() {
        let observation = ProviderObservation::probe_cli_at(
            ProviderRef::new("fixture"),
            PathBuf::from("/definitely/missing/fixture-cli").as_os_str(),
            123,
            &ProcessRunner,
        );

        assert_eq!(
            observation.cli_present,
            Evidence::Known {
                value: false,
                basis: EvidenceBasis::Measured,
                assessed_at_ms: 123,
                source: EvidenceSource {
                    kind: EvidenceSourceKind::ProviderCli,
                    reference: "fixture --version".to_owned(),
                },
            }
        );
        assert!(matches!(
            observation.availability.status,
            AvailabilityStatus::Unavailable { .. }
        ));
        assert!(matches!(
            observation.authentication.status,
            AvailabilityStatus::Unknown { .. }
        ));
        assert!(matches!(
            observation.models[0].availability.status,
            AvailabilityStatus::Unknown { .. }
        ));
        assert_eq!(observation.availability.observed_at_ms, 123);
    }

    #[test]
    fn started_but_nonzero_cli_is_present_but_not_provider_available() {
        let executable = std::env::current_exe().expect("test executable");
        let observation = ProviderObservation::probe_cli_at(
            ProviderRef::new("fixture"),
            executable.as_os_str(),
            456,
            &ProcessRunner,
        );

        assert_eq!(
            observation.cli_present,
            Evidence::Known {
                value: true,
                basis: EvidenceBasis::Measured,
                assessed_at_ms: 456,
                source: EvidenceSource {
                    kind: EvidenceSourceKind::ProviderCli,
                    reference: "fixture --version".to_owned(),
                },
            }
        );
        assert_eq!(
            observation.cli_version_check,
            Evidence::Known {
                value: false,
                basis: EvidenceBasis::Measured,
                assessed_at_ms: 456,
                source: EvidenceSource {
                    kind: EvidenceSourceKind::ProviderCli,
                    reference: "fixture --version".to_owned(),
                },
            }
        );
        assert!(matches!(
            observation.availability.status,
            AvailabilityStatus::Unknown { .. }
        ));
    }

    #[cfg(unix)]
    #[test]
    fn successful_cli_version_check_still_leaves_authentication_unknown() {
        let observation = ProviderObservation::probe_cli_at(
            ProviderRef::new("fixture"),
            OsStr::new("true"),
            654,
            &ProcessRunner,
        );

        assert_eq!(
            observation.cli_present,
            Evidence::Known {
                value: true,
                basis: EvidenceBasis::Measured,
                assessed_at_ms: 654,
                source: EvidenceSource {
                    kind: EvidenceSourceKind::ProviderCli,
                    reference: "fixture --version".to_owned(),
                },
            }
        );
        assert_eq!(
            observation.cli_version_check,
            Evidence::Known {
                value: true,
                basis: EvidenceBasis::Measured,
                assessed_at_ms: 654,
                source: EvidenceSource {
                    kind: EvidenceSourceKind::ProviderCli,
                    reference: "fixture --version".to_owned(),
                },
            }
        );
        assert!(matches!(
            observation.authentication.status,
            AvailabilityStatus::Unknown { .. }
        ));
        assert!(matches!(
            observation.availability.status,
            AvailabilityStatus::Unknown { .. }
        ));
    }

    #[test]
    fn attempt_target_keeps_requested_and_observed_model_separate() {
        let mut attempt = Attempt::new(
            AttemptId::new("attempt-1"),
            ProviderRef::new("requested-provider"),
            ModelChoice::Named(ModelRef::new("requested-model")),
        );
        attempt.record_observed_target(
            Some(ProviderRef::new("observed-provider")),
            Some(ModelRef::new("observed-model")),
        );
        let record = AttemptRecord::new(TaskId::new("task-1"), attempt, Some(100), Some(200));
        let observation = AttemptTargetObservation::from_ledger_record(&record, 789);

        assert!(matches!(
            observation.requested_model,
            Evidence::Known {
                value: ModelChoice::Named(model),
                basis: EvidenceBasis::Configured,
                ..
            }
                if model.as_str() == "requested-model"
        ));
        assert!(matches!(
            observation.observed_model,
            Evidence::Known {
                value,
                basis: EvidenceBasis::Measured,
                ..
            } if value.as_str() == "observed-model"
        ));
        assert!(matches!(
            observation.requested_provider,
            Evidence::Known { value, .. } if value.as_str() == "requested-provider"
        ));
        assert!(matches!(
            observation.observed_provider,
            Evidence::Known { value, .. } if value.as_str() == "observed-provider"
        ));
    }

    #[test]
    fn missing_observed_model_remains_unknown_in_attempt_observation() {
        let attempt = Attempt::new(
            AttemptId::new("attempt-1"),
            ProviderRef::new("provider"),
            ModelChoice::ProviderDefault,
        );
        let record = AttemptRecord::new(TaskId::new("task-1"), attempt, Some(100), None);
        let observation = AttemptTargetObservation::from_ledger_record(&record, 789);

        assert!(matches!(
            observation.requested_model,
            Evidence::Known {
                value: ModelChoice::ProviderDefault,
                ..
            }
        ));
        assert!(matches!(
            observation.observed_model,
            Evidence::Unknown {
                assessed_at_ms: 789,
                ..
            }
        ));
    }
}
