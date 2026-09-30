//! Typed current observations for Provider CLI launchability.
//!
//! This module deliberately does not infer account authentication, model access,
//! quota, billing, or API usage from a local CLI version command. Those facts
//! remain unknown until a Provider exposes an authoritative source.

use std::{
    collections::HashMap,
    ffi::OsStr,
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use crate::{
    AttemptRecord, ModelChoice, ModelRef, ProcessError, ProcessRequest, ProcessRunner, ProviderRef,
    StopReason,
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

/// Safe, fixed categories for a failed attempt to obtain a current observation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ObservationFetchFailureReason {
    StartFailed,
    TimedOut,
    Interrupted { reason: StopReason, stopped: bool },
    Cancelled,
    IoFailure,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LatestObservationFetch {
    Succeeded {
        observed_at_ms: i64,
        source: EvidenceSource,
    },
    Failed {
        reason: ObservationFetchFailureReason,
        failed_at_ms: i64,
        source: EvidenceSource,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ObservationFreshness {
    Current,
    Stale { since_ms: i64 },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LastSuccessfulProviderObservation {
    pub observation: ProviderObservation,
    pub freshness: ObservationFreshness,
}

/// Latest fetch outcome and the latest successful observation, if one exists.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CachedProviderObservation {
    pub latest_fetch: LatestObservationFetch,
    pub last_successful: Option<LastSuccessfulProviderObservation>,
}

/// Process-local cache. Entries are intentionally not persisted and reset on restart.
#[derive(Debug, Default)]
pub struct ProviderObservationCache {
    entries: Mutex<HashMap<ObservationCacheKey, CacheEntry>>,
    next_refresh_id: AtomicU64,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct ObservationCacheKey {
    provider: ProviderRef,
    executable: std::ffi::OsString,
}

#[derive(Clone, Debug)]
struct CacheEntry {
    refresh_id: u64,
    observation: CachedProviderObservation,
}

impl ProviderObservationCache {
    #[must_use]
    pub fn get(
        &self,
        provider: &ProviderRef,
        executable: &OsStr,
    ) -> Option<CachedProviderObservation> {
        self.entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&ObservationCacheKey {
                provider: provider.clone(),
                executable: executable.to_os_string(),
            })
            .map(|entry| entry.observation.clone())
    }

    /// Fetches a Provider CLI observation and retains the last successful result
    /// separately if this fetch fails.
    pub fn refresh_cli_at(
        &self,
        provider: ProviderRef,
        executable: &OsStr,
        observed_at_ms: i64,
        runner: &ProcessRunner,
    ) -> CachedProviderObservation {
        let refresh_id = self.next_refresh_id.fetch_add(1, Ordering::Relaxed);
        let key = ObservationCacheKey {
            provider: provider.clone(),
            executable: executable.to_os_string(),
        };
        let source = cli_probe_source(&provider);
        let fetched = ProviderObservation::try_probe_cli_at(
            provider.clone(),
            executable,
            observed_at_ms,
            runner,
        );
        let mut entries = self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(entry) = entries.get(&key) {
            if entry.refresh_id > refresh_id {
                return entry.observation.clone();
            }
        }
        let previous_success = entries
            .get(&key)
            .and_then(|entry| entry.observation.last_successful.clone());
        let next = match fetched {
            Ok(observation) => CachedProviderObservation {
                latest_fetch: LatestObservationFetch::Succeeded {
                    observed_at_ms,
                    source,
                },
                last_successful: Some(LastSuccessfulProviderObservation {
                    observation,
                    freshness: ObservationFreshness::Current,
                }),
            },
            Err(reason) => {
                let stale_since_ms =
                    previous_success
                        .as_ref()
                        .map_or(observed_at_ms, |last| match last.freshness {
                            ObservationFreshness::Current => observed_at_ms,
                            ObservationFreshness::Stale { since_ms } => since_ms,
                        });
                CachedProviderObservation {
                    latest_fetch: LatestObservationFetch::Failed {
                        reason,
                        failed_at_ms: observed_at_ms,
                        source,
                    },
                    last_successful: previous_success.map(|mut last| {
                        last.freshness = ObservationFreshness::Stale {
                            since_ms: stale_since_ms,
                        };
                        last
                    }),
                }
            }
        };
        entries.insert(
            key,
            CacheEntry {
                refresh_id,
                observation: next.clone(),
            },
        );
        next
    }
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
        let result = runner.run(
            ProcessRequest::new(executable.to_os_string())
                .arg("--version")
                .timeout(CLI_PROBE_TIMEOUT),
        );
        Self::from_cli_result(provider, observed_at_ms, result)
    }

    /// Fetches a current CLI observation, returning operational fetch failures
    /// separately from successful probes that report unknown facts.
    pub fn try_probe_cli_at(
        provider: ProviderRef,
        executable: &OsStr,
        observed_at_ms: i64,
        runner: &ProcessRunner,
    ) -> Result<Self, ObservationFetchFailureReason> {
        let result = runner.run(
            ProcessRequest::new(executable.to_os_string())
                .arg("--version")
                .timeout(CLI_PROBE_TIMEOUT),
        );
        if let Err(error) = &result {
            if let Some(reason) = fetch_failure_reason(error) {
                return Err(reason);
            }
        }
        Ok(Self::from_cli_result(provider, observed_at_ms, result))
    }

    fn from_cli_result(
        provider: ProviderRef,
        observed_at_ms: i64,
        result: Result<crate::ProcessOutput, ProcessError>,
    ) -> Self {
        let source = cli_probe_source(&provider);

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

fn cli_probe_source(provider: &ProviderRef) -> EvidenceSource {
    EvidenceSource {
        kind: EvidenceSourceKind::ProviderCli,
        reference: format!("{} --version", provider.as_str()),
    }
}

fn fetch_failure_reason(error: &ProcessError) -> Option<ObservationFetchFailureReason> {
    match error {
        ProcessError::Spawn(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        ProcessError::Spawn(_) => Some(ObservationFetchFailureReason::StartFailed),
        ProcessError::TimedOut(_) => Some(ObservationFetchFailureReason::TimedOut),
        ProcessError::Interrupted {
            reason, stopped, ..
        } => Some(ObservationFetchFailureReason::Interrupted {
            reason: *reason,
            stopped: *stopped,
        }),
        ProcessError::Cancelled(_) | ProcessError::CancelledBeforeStart => {
            Some(ObservationFetchFailureReason::Cancelled)
        }
        ProcessError::Io(_) => Some(ObservationFetchFailureReason::IoFailure),
        ProcessError::NonZeroExit(_) => None,
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

    #[cfg(unix)]
    #[test]
    fn cache_keeps_last_success_stale_after_a_later_probe_failure() {
        use std::os::unix::fs::PermissionsExt;

        let provider = ProviderRef::new("fixture");
        let cache = ProviderObservationCache::default();
        let executable = std::env::temp_dir().join(format!(
            "provider-observation-fixture-{}",
            std::process::id()
        ));
        std::fs::write(&executable, "#!/bin/sh\nprintf 'fixture version\\n'\n").unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
        let initial = cache.refresh_cli_at(
            provider.clone(),
            executable.as_os_str(),
            1_000,
            &ProcessRunner,
        );
        assert!(matches!(
            initial.latest_fetch,
            LatestObservationFetch::Succeeded {
                observed_at_ms: 1_000,
                source,
            }
                if source.reference == "fixture --version"
        ));
        let original = initial.last_successful.unwrap().observation;

        assert!(
            cache
                .get(&provider, std::env::current_exe().unwrap().as_os_str())
                .is_none()
        );
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o600)).unwrap();

        let failed = cache.refresh_cli_at(
            provider.clone(),
            executable.as_os_str(),
            2_000,
            &ProcessRunner,
        );
        assert_eq!(
            failed.latest_fetch,
            LatestObservationFetch::Failed {
                reason: ObservationFetchFailureReason::StartFailed,
                failed_at_ms: 2_000,
                source: cli_probe_source(&provider),
            }
        );
        assert!(
            cache
                .get(&provider, executable.as_os_str())
                .unwrap()
                .last_successful
                .is_some()
        );
        let stale = failed.last_successful.unwrap();
        assert_eq!(stale.observation, original);
        assert_eq!(
            stale.freshness,
            ObservationFreshness::Stale { since_ms: 2_000 }
        );

        let later_failure = cache.refresh_cli_at(
            provider.clone(),
            executable.as_os_str(),
            3_000,
            &ProcessRunner,
        );
        assert_eq!(
            later_failure.last_successful.unwrap().freshness,
            ObservationFreshness::Stale { since_ms: 2_000 }
        );
        let _ = std::fs::remove_file(&executable);

        std::fs::write(&executable, "#!/bin/sh\nprintf 'fixture version\\n'\n").unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
        let recovered =
            cache.refresh_cli_at(provider, executable.as_os_str(), 4_000, &ProcessRunner);
        assert!(matches!(
            recovered.latest_fetch,
            LatestObservationFetch::Succeeded {
                observed_at_ms: 4_000,
                ..
            }
        ));
        assert_eq!(
            recovered.last_successful.unwrap().freshness,
            ObservationFreshness::Current
        );
        let _ = std::fs::remove_file(executable);
    }

    #[test]
    fn interrupted_fetch_reason_preserves_stop_reason_and_confirmation_without_logs() {
        let error = ProcessError::Interrupted {
            reason: StopReason::PipeHeld,
            stopped: false,
            stdout: b"sensitive stdout".to_vec(),
            stderr: b"sensitive stderr".to_vec(),
            diagnostic: "sensitive diagnostic".to_owned(),
        };

        assert_eq!(
            fetch_failure_reason(&error),
            Some(ObservationFetchFailureReason::Interrupted {
                reason: StopReason::PipeHeld,
                stopped: false,
            })
        );
        assert_eq!(
            fetch_failure_reason(&ProcessError::Interrupted {
                reason: StopReason::TimedOut,
                stopped: true,
                stdout: Vec::new(),
                stderr: Vec::new(),
                diagnostic: String::new(),
            }),
            Some(ObservationFetchFailureReason::Interrupted {
                reason: StopReason::TimedOut,
                stopped: true,
            })
        );
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
