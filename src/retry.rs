//! Retry and provider-resolution policy owned by the orchestrator.

use std::{collections::HashMap, time::Duration};

use crate::{AgentProvider, ProviderError, ProviderRef};

/// Errors raised by the hard execution policy before any domain or external mutation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PolicyError {
    MaxAttemptsZero,
    MaxAttemptsReached {
        max_attempts: usize,
    },
    TimeoutMissing {
        provider: ProviderRef,
    },
    TimeoutZero {
        provider: ProviderRef,
    },
    ProviderUnavailable {
        provider: ProviderRef,
        reason: String,
    },
    UnknownProvider {
        provider: ProviderRef,
    },
    TaskClosed {
        state: crate::TaskState,
    },
    DuplicateAttempt {
        id: crate::AttemptId,
    },
}

impl std::fmt::Display for PolicyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MaxAttemptsZero => f.write_str("max_attempts must be greater than zero"),
            Self::MaxAttemptsReached { max_attempts } => {
                write!(f, "maximum attempts reached: {max_attempts}")
            }
            Self::TimeoutMissing { provider } => write!(
                f,
                "no timeout configured for provider {}",
                provider.as_str()
            ),
            Self::TimeoutZero { provider } => write!(
                f,
                "timeout must be greater than zero for provider {}",
                provider.as_str()
            ),
            Self::ProviderUnavailable { provider, reason } => {
                write!(f, "provider {} unavailable: {reason}", provider.as_str())
            }
            Self::UnknownProvider { provider } => {
                write!(f, "unknown provider: {}", provider.as_str())
            }
            Self::TaskClosed { state } => write!(f, "task is closed in state {state:?}"),
            Self::DuplicateAttempt { id } => write!(f, "duplicate attempt id: {}", id.as_str()),
        }
    }
}
impl std::error::Error for PolicyError {}

/// Rust-owned limits for planner-driven execution.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExecutionPolicy {
    max_attempts: usize,
    timeouts: HashMap<ProviderRef, Duration>,
}

impl ExecutionPolicy {
    /// Constructs a policy, rejecting an unusable zero-attempt limit.
    pub fn new(max_attempts: usize) -> Result<Self, PolicyError> {
        if max_attempts == 0 {
            return Err(PolicyError::MaxAttemptsZero);
        }
        Ok(Self {
            max_attempts,
            timeouts: HashMap::new(),
        })
    }

    pub fn try_new(max_attempts: usize) -> Result<Self, PolicyError> {
        if max_attempts == 0 {
            Err(PolicyError::MaxAttemptsZero)
        } else {
            Self::new(max_attempts)
        }
    }

    #[must_use]
    pub const fn max_attempts(&self) -> usize {
        self.max_attempts
    }

    /// Adds or replaces a provider timeout, rejecting zero immediately.
    pub fn with_timeout(
        mut self,
        provider: impl Into<ProviderRef>,
        timeout: Duration,
    ) -> Result<Self, PolicyError> {
        let provider = provider.into();
        if timeout.is_zero() {
            return Err(PolicyError::TimeoutZero { provider });
        }
        self.timeouts.insert(provider, timeout);
        Ok(self)
    }

    pub fn timeout_for(&self, provider: &ProviderRef) -> Result<Duration, PolicyError> {
        if self.max_attempts == 0 {
            return Err(PolicyError::MaxAttemptsZero);
        }
        let timeout =
            self.timeouts
                .get(provider)
                .copied()
                .ok_or_else(|| PolicyError::TimeoutMissing {
                    provider: provider.clone(),
                })?;
        if timeout.is_zero() {
            return Err(PolicyError::TimeoutZero {
                provider: provider.clone(),
            });
        }
        Ok(timeout)
    }
}

/// Explicit policy for whether a failed attempt may be followed by another one.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RetryPolicy {
    max_attempts: usize,
    retry_on_timeout: bool,
    retry_on_cancellation: bool,
}

impl RetryPolicy {
    #[must_use]
    pub const fn new(max_attempts: usize) -> Self {
        Self {
            max_attempts,
            retry_on_timeout: false,
            retry_on_cancellation: false,
        }
    }
    pub const fn try_new(max_attempts: usize) -> Result<Self, &'static str> {
        if max_attempts == 0 {
            Err("max_attempts must be greater than zero")
        } else {
            Ok(Self::new(max_attempts))
        }
    }
    #[must_use]
    pub const fn is_valid(&self) -> bool {
        self.max_attempts > 0
    }
    #[must_use]
    pub const fn max_attempts(&self) -> usize {
        self.max_attempts
    }
    #[must_use]
    pub const fn with_timeout_retry(mut self, enabled: bool) -> Self {
        self.retry_on_timeout = enabled;
        self
    }
    #[must_use]
    pub const fn with_cancellation_retry(mut self, enabled: bool) -> Self {
        self.retry_on_cancellation = enabled;
        self
    }
    #[must_use]
    pub const fn allows(&self, error: &ProviderError) -> bool {
        match error {
            ProviderError::TimedOut { .. } => self.retry_on_timeout,
            ProviderError::Cancelled => self.retry_on_cancellation,
            _ => true,
        }
    }
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self::new(1)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProviderResolutionError {
    UnknownProvider { provider: ProviderRef },
}

impl std::fmt::Display for ProviderResolutionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownProvider { provider } => {
                write!(f, "unknown provider: {}", provider.as_str())
            }
        }
    }
}
impl std::error::Error for ProviderResolutionError {}

/// Registry boundary; applications can replace it with a deterministic fake.
pub trait ProviderResolver {
    fn resolve(
        &self,
        provider: &ProviderRef,
    ) -> Result<&dyn AgentProvider, ProviderResolutionError>;
}

/// In-memory provider registry for production wiring and tests.
pub struct ProviderRegistry {
    providers: Vec<Box<dyn AgentProvider>>,
}
impl std::fmt::Debug for ProviderRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProviderRegistry")
            .field("count", &self.providers.len())
            .finish()
    }
}
impl Default for ProviderRegistry {
    fn default() -> Self {
        Self::new()
    }
}
impl ProviderRegistry {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            providers: Vec::new(),
        }
    }
    pub fn register(&mut self, provider: impl AgentProvider + 'static) {
        self.providers.push(Box::new(provider));
    }
}
impl ProviderResolver for ProviderRegistry {
    fn resolve(
        &self,
        provider: &ProviderRef,
    ) -> Result<&dyn AgentProvider, ProviderResolutionError> {
        self.providers
            .iter()
            .find(|p| p.provider_ref() == provider)
            .map(|p| p.as_ref())
            .ok_or_else(|| ProviderResolutionError::UnknownProvider {
                provider: provider.clone(),
            })
    }
}

/// A single provider is also a resolver, preserving the original API shape.
impl AgentProvider for ProviderRegistry {
    fn provider_ref(&self) -> &ProviderRef {
        self.providers.first().map_or_else(
            || {
                static EMPTY: std::sync::OnceLock<ProviderRef> = std::sync::OnceLock::new();
                EMPTY.get_or_init(|| ProviderRef::new("registry"))
            },
            |provider| provider.provider_ref(),
        )
    }
    fn execute(
        &self,
        _request: &crate::ProviderRequest,
    ) -> Result<crate::ProviderResult, crate::ProviderError> {
        Err(crate::ProviderError::Unavailable(
            "registry requires a selected provider".to_owned(),
        ))
    }
    fn check_availability(&self) -> Result<(), crate::ProviderError> {
        Err(crate::ProviderError::Unavailable(
            "registry requires a selected provider".to_owned(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ProviderRequest, ProviderResult};
    use std::time::Duration;

    struct Fake(ProviderRef);
    impl AgentProvider for Fake {
        fn provider_ref(&self) -> &ProviderRef {
            &self.0
        }
        fn execute(&self, _: &ProviderRequest) -> Result<ProviderResult, ProviderError> {
            Err(ProviderError::ExecutionFailed("fake".into()))
        }
        fn check_availability(&self) -> Result<(), ProviderError> {
            Ok(())
        }
    }

    #[test]
    fn timeout_and_cancel_retry_are_explicit() {
        let timeout = ProviderError::TimedOut {
            timeout: Duration::from_secs(1),
        };
        assert!(!RetryPolicy::new(2).allows(&timeout));
        assert!(
            RetryPolicy::new(2)
                .with_timeout_retry(true)
                .allows(&timeout)
        );
        assert!(!RetryPolicy::new(2).allows(&ProviderError::Cancelled));
    }

    #[test]
    fn registry_resolves_and_rejects_providers_with_typed_error() {
        let mut registry = ProviderRegistry::new();
        registry.register(Fake(ProviderRef::new("one")));
        assert_eq!(
            registry
                .resolve(&ProviderRef::new("one"))
                .unwrap()
                .provider_ref()
                .as_str(),
            "one"
        );
        assert!(matches!(
            registry.resolve(&ProviderRef::new("missing")),
            Err(ProviderResolutionError::UnknownProvider { .. })
        ));
    }
}
