//! Repository-local configuration for deterministic validation checks.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::{
    CancellationToken, CommandValidator, ValidationCheck, ValidationResult, Validator,
    ValidatorError,
};

/// Canonical configuration path, relative to a repository root.
pub const REPOSITORY_CONFIG_PATH: &str = ".ai-dev-orchestrator/config.toml";

const TEMPLATE: &str = "# Repository-local mechanical validation. Checks run in listed order.\n\
schema_version = 1\n\
\n\
[validation]\n\
# Add repository checks explicitly; an empty list is never treated as success.\n\
checks = []\n";

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct RepositoryConfig {
    pub schema_version: u32,
    #[serde(default)]
    pub validation: Option<ValidationConfig>,
    #[serde(skip)]
    config_version: String,
}

/// Validator selected from one loaded repository configuration snapshot.
#[derive(Clone, Debug)]
pub struct RepositoryConfiguredValidator {
    inner: CommandValidator,
    config_id: String,
    config_version: String,
}

impl Validator for RepositoryConfiguredValidator {
    fn validate(&self, workspace: &Path) -> Result<ValidationResult, ValidatorError> {
        self.validate_with_cancellation(workspace, CancellationToken::new())
    }

    fn validate_with_cancellation(
        &self,
        workspace: &Path,
        token: CancellationToken,
    ) -> Result<ValidationResult, ValidatorError> {
        RepositoryConfiguredValidator::validate_with_cancellation(self, workspace, token)
    }
}

impl RepositoryConfiguredValidator {
    /// Runs configured checks using the caller's cancellation signal.
    pub fn validate_with_cancellation(
        &self,
        workspace: &Path,
        token: CancellationToken,
    ) -> Result<ValidationResult, ValidatorError> {
        self.inner
            .validate_with_cancellation(workspace, token)
            .map(|result| {
                result.with_config_identity(self.config_id.clone(), self.config_version.clone())
            })
    }
}

#[derive(Deserialize)]
struct SchemaVersionHeader {
    schema_version: u32,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ValidationConfig {
    #[serde(default)]
    pub checks: Vec<ValidationCheckConfig>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ValidationCheckConfig {
    pub name: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub cwd: Option<PathBuf>,
    pub timeout_ms: u64,
}

#[derive(Debug)]
pub enum RepositoryConfigError {
    Io(std::io::Error),
    Parse(toml::de::Error),
    UnsupportedVersion(u32),
    MissingSourceVersion,
    NoChecksConfigured,
    InvalidCheck { name: String, reason: &'static str },
}

impl std::fmt::Display for RepositoryConfigError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "repository config I/O error: {error}"),
            Self::Parse(error) => write!(formatter, "invalid repository config: {error}"),
            Self::UnsupportedVersion(version) => {
                write!(
                    formatter,
                    "unsupported repository config schema_version {version}"
                )
            }
            Self::MissingSourceVersion => {
                write!(formatter, "repository config was not loaded from disk")
            }
            Self::NoChecksConfigured => {
                write!(formatter, "no validation checks configured")
            }
            Self::InvalidCheck { name, reason } => {
                write!(formatter, "invalid validation check '{name}': {reason}")
            }
        }
    }
}

impl std::error::Error for RepositoryConfigError {}

impl RepositoryConfig {
    /// Creates a command validator from the configured checks in declaration order.
    pub fn command_validator(&self) -> Result<CommandValidator, RepositoryConfigError> {
        let configured_checks = self
            .validation
            .as_ref()
            .map_or(&[][..], |validation| validation.checks.as_slice());
        if configured_checks.is_empty() {
            return Err(RepositoryConfigError::NoChecksConfigured);
        }
        let checks = configured_checks
            .iter()
            .map(|configured| {
                if configured.name.trim().is_empty() {
                    return Err(RepositoryConfigError::InvalidCheck {
                        name: configured.name.clone(),
                        reason: "name must not be empty",
                    });
                }
                if configured.command.trim().is_empty() {
                    return Err(RepositoryConfigError::InvalidCheck {
                        name: configured.name.clone(),
                        reason: "command must not be empty",
                    });
                }
                if configured.timeout_ms == 0 {
                    return Err(RepositoryConfigError::InvalidCheck {
                        name: configured.name.clone(),
                        reason: "timeout_ms must be greater than zero",
                    });
                }
                if configured.cwd.as_ref().is_some_and(|cwd| {
                    cwd.components().any(|component| {
                        matches!(
                            component,
                            std::path::Component::RootDir
                                | std::path::Component::Prefix(_)
                                | std::path::Component::ParentDir
                        )
                    })
                }) {
                    return Err(RepositoryConfigError::InvalidCheck {
                        name: configured.name.clone(),
                        reason: "cwd must be a relative path without parent components",
                    });
                }
                let mut check = ValidationCheck::new(&configured.name, &configured.command)
                    .args(configured.args.iter().map(String::as_str))
                    .timeout(Duration::from_millis(configured.timeout_ms));
                if let Some(cwd) = &configured.cwd {
                    check = check.cwd(cwd);
                }
                Ok(check)
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(CommandValidator::new(checks))
    }

    /// Builds the Validator used for normal repository execution and records its source version.
    pub fn validator(&self) -> Result<RepositoryConfiguredValidator, RepositoryConfigError> {
        if self.config_version.is_empty() {
            return Err(RepositoryConfigError::MissingSourceVersion);
        }
        Ok(RepositoryConfiguredValidator {
            inner: self.command_validator()?,
            config_id: REPOSITORY_CONFIG_PATH.to_owned(),
            config_version: self.config_version.clone(),
        })
    }
}

/// Writes the initial template without replacing existing repository data.
pub fn init_repository(repository_root: &Path) -> Result<PathBuf, RepositoryConfigError> {
    let root = fs::canonicalize(repository_root).map_err(RepositoryConfigError::Io)?;
    if !root.is_dir() {
        return Err(RepositoryConfigError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "repository root must be a directory",
        )));
    }
    let path = root.join(REPOSITORY_CONFIG_PATH);
    let parent = path.parent().expect("config path has a parent");
    match fs::symlink_metadata(parent) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err(RepositoryConfigError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "repository config directory must not be a symlink",
            )));
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir(parent).map_err(RepositoryConfigError::Io)?;
        }
        Err(error) => return Err(RepositoryConfigError::Io(error)),
    }
    let canonical_parent = fs::canonicalize(parent).map_err(RepositoryConfigError::Io)?;
    if !canonical_parent.starts_with(&root) {
        return Err(RepositoryConfigError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "repository config directory resolves outside the repository",
        )));
    }
    let path = canonical_parent.join("config.toml");
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .map_err(RepositoryConfigError::Io)?;
    file.write_all(TEMPLATE.as_bytes())
        .map_err(RepositoryConfigError::Io)?;
    file.sync_all().map_err(RepositoryConfigError::Io)?;
    Ok(path)
}

/// Loads the canonical config path. A missing file is a configuration error.
pub fn load_repository_config(
    repository_root: &Path,
) -> Result<RepositoryConfig, RepositoryConfigError> {
    let root = fs::canonicalize(repository_root).map_err(RepositoryConfigError::Io)?;
    if !root.is_dir() {
        return Err(RepositoryConfigError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "repository root must be a directory",
        )));
    }
    let path = root.join(REPOSITORY_CONFIG_PATH);
    let parent = path.parent().expect("config path has a parent");
    let parent_metadata = fs::symlink_metadata(parent).map_err(RepositoryConfigError::Io)?;
    if parent_metadata.file_type().is_symlink() {
        return Err(RepositoryConfigError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "repository config directory must not be a symlink",
        )));
    }
    let canonical_parent = fs::canonicalize(parent).map_err(RepositoryConfigError::Io)?;
    if !canonical_parent.starts_with(&root) {
        return Err(RepositoryConfigError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "repository config directory resolves outside the repository",
        )));
    }
    let config_path = canonical_parent.join("config.toml");
    let config_metadata = fs::symlink_metadata(&config_path).map_err(RepositoryConfigError::Io)?;
    if config_metadata.file_type().is_symlink() {
        return Err(RepositoryConfigError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "repository config file must not be a symlink",
        )));
    }
    if !config_metadata.is_file() {
        return Err(RepositoryConfigError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "repository config path must be a file",
        )));
    }
    let canonical_config = fs::canonicalize(&config_path).map_err(RepositoryConfigError::Io)?;
    if !canonical_config.starts_with(&canonical_parent) {
        return Err(RepositoryConfigError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "repository config file resolves outside the repository",
        )));
    }
    // Read the resolved in-repository path rather than the original path, which could
    // otherwise traverse a static directory or file symlink outside the repository.
    // Standard-library path checks do not provide descriptor-relative open semantics,
    // so concurrent replacement of these paths remains outside this guarantee.
    let source = fs::read_to_string(canonical_config).map_err(RepositoryConfigError::Io)?;
    let header: SchemaVersionHeader =
        toml::from_str(&source).map_err(RepositoryConfigError::Parse)?;
    if header.schema_version != 1 {
        return Err(RepositoryConfigError::UnsupportedVersion(
            header.schema_version,
        ));
    }
    // Keep version 1 strict, including rejection of fields introduced by later schemas.
    let mut config: RepositoryConfig =
        toml::from_str(&source).map_err(RepositoryConfigError::Parse)?;
    // Validate before a caller can start any subprocess.
    config.command_validator()?;
    let digest = Sha256::digest(source.as_bytes());
    config.config_version = digest.iter().map(|byte| format!("{byte:02x}")).collect();
    Ok(config)
}
