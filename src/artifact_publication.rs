//! Fail-closed publication of an immutable Artifact tree as a GitHub Draft PR.

use std::{
    fmt,
    fs::{self, OpenOptions},
    io::Write,
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

use serde_json::{Value, json};

use crate::process_runner::{ProcessRequest, ProcessRunner};

static TEMP_PAYLOAD_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// The public information that will be sent to GitHub for a Pull Request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ArtifactPublicationPayload {
    base_branch: String,
    head_branch: String,
    title: String,
    body: String,
}

impl ArtifactPublicationPayload {
    #[must_use]
    pub fn new(
        base_branch: impl Into<String>,
        head_branch: impl Into<String>,
        title: impl Into<String>,
        body: impl Into<String>,
    ) -> Self {
        Self {
            base_branch: base_branch.into(),
            head_branch: head_branch.into(),
            title: title.into(),
            body: body.into(),
        }
    }

    #[must_use]
    pub fn base_branch(&self) -> &str {
        &self.base_branch
    }
    #[must_use]
    pub fn head_branch(&self) -> &str {
        &self.head_branch
    }
    #[must_use]
    pub fn title(&self) -> &str {
        &self.title
    }
    #[must_use]
    pub fn body(&self) -> &str {
        &self.body
    }
}

/// Result of scanning one complete immutable Artifact or publication payload.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SecretScanResult {
    Clean,
    Findings,
}

/// A scanner must inspect every file in the supplied Git tree and every field in the payload.
/// Implementations should return only a typed failure; diagnostics may contain secret material.
pub trait SecretScanner: Send + Sync {
    fn scan_artifact_tree(
        &self,
        repository: &Path,
        tree_oid: &str,
    ) -> Result<SecretScanResult, SecretScanError>;

    fn scan_publication_payload(
        &self,
        payload: &ArtifactPublicationPayload,
    ) -> Result<SecretScanResult, SecretScanError>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SecretScanError {
    Failed,
    Unavailable,
}

impl fmt::Display for SecretScanError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Failed => "secret scan failed",
            Self::Unavailable => "secret scanner unavailable",
        })
    }
}

impl std::error::Error for SecretScanError {}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DraftPullRequest {
    number: u64,
    url: String,
    draft: bool,
    head_sha: String,
    head_branch: String,
    base_branch: String,
}

impl DraftPullRequest {
    #[must_use]
    pub fn new(
        number: u64,
        url: impl Into<String>,
        draft: bool,
        head_sha: impl Into<String>,
        head_branch: impl Into<String>,
        base_branch: impl Into<String>,
    ) -> Self {
        Self {
            number,
            url: url.into(),
            draft,
            head_sha: head_sha.into(),
            head_branch: head_branch.into(),
            base_branch: base_branch.into(),
        }
    }

    #[must_use]
    pub const fn number(&self) -> u64 {
        self.number
    }
    #[must_use]
    pub fn url(&self) -> &str {
        &self.url
    }
    #[must_use]
    pub const fn is_draft(&self) -> bool {
        self.draft
    }
    #[must_use]
    pub fn head_sha(&self) -> &str {
        &self.head_sha
    }
    #[must_use]
    pub fn head_branch(&self) -> &str {
        &self.head_branch
    }
    #[must_use]
    pub fn base_branch(&self) -> &str {
        &self.base_branch
    }
}

/// Git and GitHub effects for an already-scanned Artifact.
pub trait ArtifactPublicationGateway: Send + Sync {
    fn commit_tree(
        &self,
        repository: &Path,
        tree_oid: &str,
        base_commit: &str,
        message: &str,
        timeout: Duration,
    ) -> Result<String, PublicationGatewayError>;

    fn push_commit(
        &self,
        repository: &Path,
        commit_sha: &str,
        head_branch: &str,
        timeout: Duration,
    ) -> Result<(), PublicationGatewayError>;

    fn create_or_find_draft_pull_request(
        &self,
        repository: &Path,
        payload: &ArtifactPublicationPayload,
        commit_sha: &str,
        timeout: Duration,
    ) -> Result<DraftPullRequest, PublicationGatewayError>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PublicationGatewayError {
    Spawn,
    CommandFailed,
    InvalidResponse,
    ExistingPullRequestMismatch,
}

impl fmt::Display for PublicationGatewayError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Spawn => "publication command could not start",
            Self::CommandFailed => "publication command failed",
            Self::InvalidResponse => "publication command returned an invalid response",
            Self::ExistingPullRequestMismatch => {
                "an existing Pull Request does not match this publication"
            }
        })
    }
}

impl std::error::Error for PublicationGatewayError {}

/// GitHub CLI adapter. PR creation always requests Draft status and verifies the returned head.
#[derive(Clone, Debug)]
pub struct GitHubArtifactPublicationGateway {
    git_executable: PathBuf,
    gh_executable: PathBuf,
}

impl Default for GitHubArtifactPublicationGateway {
    fn default() -> Self {
        Self::new()
    }
}

impl GitHubArtifactPublicationGateway {
    #[must_use]
    pub fn new() -> Self {
        Self::with_executables("git", "gh")
    }

    #[must_use]
    pub fn with_executables(
        git_executable: impl Into<PathBuf>,
        gh_executable: impl Into<PathBuf>,
    ) -> Self {
        Self {
            git_executable: git_executable.into(),
            gh_executable: gh_executable.into(),
        }
    }

    fn git_output(
        &self,
        repository: &Path,
        args: &[&str],
        timeout: Duration,
    ) -> Result<String, PublicationGatewayError> {
        let mut request = ProcessRequest::new(self.git_executable.clone()).timeout(timeout);
        request.args = vec![
            std::ffi::OsString::from("-C"),
            repository.as_os_str().to_owned(),
        ];
        request
            .args
            .extend(args.iter().map(std::ffi::OsString::from));
        let output = ProcessRunner.run(request).map_err(map_process_error)?;
        if output.output_truncated {
            return Err(PublicationGatewayError::InvalidResponse);
        }
        String::from_utf8(output.stdout)
            .map(|value| value.trim().to_owned())
            .map_err(|_| PublicationGatewayError::InvalidResponse)
    }

    fn gh_output(
        &self,
        repository: &Path,
        args: &[&str],
        timeout: Duration,
    ) -> Result<String, PublicationGatewayError> {
        let mut request = ProcessRequest::new(self.gh_executable.clone())
            .cwd(repository)
            .timeout(timeout);
        request.args = args.iter().map(std::ffi::OsString::from).collect();
        let output = ProcessRunner.run(request).map_err(map_process_error)?;
        if output.output_truncated {
            return Err(PublicationGatewayError::InvalidResponse);
        }
        String::from_utf8(output.stdout)
            .map(|value| value.trim().to_owned())
            .map_err(|_| PublicationGatewayError::InvalidResponse)
    }
}

impl ArtifactPublicationGateway for GitHubArtifactPublicationGateway {
    fn commit_tree(
        &self,
        repository: &Path,
        tree_oid: &str,
        base_commit: &str,
        message: &str,
        timeout: Duration,
    ) -> Result<String, PublicationGatewayError> {
        self.git_output(
            repository,
            &["commit-tree", tree_oid, "-p", base_commit, "-m", message],
            timeout,
        )
    }

    fn push_commit(
        &self,
        repository: &Path,
        commit_sha: &str,
        head_branch: &str,
        timeout: Duration,
    ) -> Result<(), PublicationGatewayError> {
        let refspec = format!("{commit_sha}:refs/heads/{head_branch}");
        self.git_output(
            repository,
            &["push", "--porcelain", "origin", &refspec],
            timeout,
        )?;
        Ok(())
    }

    fn create_or_find_draft_pull_request(
        &self,
        repository: &Path,
        payload: &ArtifactPublicationPayload,
        commit_sha: &str,
        timeout: Duration,
    ) -> Result<DraftPullRequest, PublicationGatewayError> {
        let listed = self.gh_output(
            repository,
            &[
                "pr",
                "list",
                "--state",
                "all",
                "--head",
                payload.head_branch(),
                "--base",
                payload.base_branch(),
                "--json",
                "number,url,isDraft,headRefOid,headRefName,baseRefName,title,body",
                "--limit",
                "2",
            ],
            timeout,
        )?;
        let values: Vec<Value> =
            serde_json::from_str(&listed).map_err(|_| PublicationGatewayError::InvalidResponse)?;
        if values.len() > 1 {
            return Err(PublicationGatewayError::ExistingPullRequestMismatch);
        }
        if let Some(value) = values.first() {
            let existing = parse_pull_request_value(value)?;
            verify_pull_request_content(value, payload)?;
            return verify_pull_request(existing, payload, commit_sha);
        }

        let repo = self.gh_output(
            repository,
            &[
                "repo",
                "view",
                "--json",
                "nameWithOwner",
                "--jq",
                ".nameWithOwner",
            ],
            timeout,
        )?;
        if !valid_repository_slug(&repo) {
            return Err(PublicationGatewayError::InvalidResponse);
        }
        let request_body = json!({
            "base": payload.base_branch(),
            "head": payload.head_branch(),
            "title": payload.title(),
            "body": payload.body(),
            "draft": true,
        });
        let input_file = TemporaryPayloadFile::create(&request_body)?;
        let endpoint = format!("repos/{repo}/pulls");
        let created = self.gh_output(
            repository,
            &[
                "api",
                &endpoint,
                "--method",
                "POST",
                "--input",
                input_file
                    .path()
                    .to_str()
                    .ok_or(PublicationGatewayError::InvalidResponse)?,
            ],
            timeout,
        );
        let cleanup = input_file.remove();
        let created = created?;
        cleanup?;
        let created_value: Value =
            serde_json::from_str(&created).map_err(|_| PublicationGatewayError::InvalidResponse)?;
        verify_pull_request_content(&created_value, payload)?;
        verify_pull_request(parse_api_pull_request(&created)?, payload, commit_sha)
    }
}

fn map_process_error(error: crate::process_runner::ProcessError) -> PublicationGatewayError {
    match error {
        crate::process_runner::ProcessError::Spawn(_) => PublicationGatewayError::Spawn,
        _ => PublicationGatewayError::CommandFailed,
    }
}

fn valid_repository_slug(value: &str) -> bool {
    let mut components = value.split('/');
    components.next().is_some_and(valid_repository_component)
        && components.next().is_some_and(valid_repository_component)
        && components.next().is_none()
}

fn valid_repository_component(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

struct TemporaryPayloadFile {
    path: PathBuf,
}

impl TemporaryPayloadFile {
    fn create(value: &Value) -> Result<Self, PublicationGatewayError> {
        let bytes =
            serde_json::to_vec(value).map_err(|_| PublicationGatewayError::InvalidResponse)?;
        for _ in 0..8 {
            let sequence = TEMP_PAYLOAD_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "ai-dev-orchestrator-publication-{}-{sequence}.json",
                std::process::id()
            ));
            let mut file = match OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&path)
            {
                Ok(file) => file,
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(_) => return Err(PublicationGatewayError::CommandFailed),
            };
            if file.write_all(&bytes).is_err() || file.sync_all().is_err() {
                drop(file);
                let _ = fs::remove_file(&path);
                return Err(PublicationGatewayError::CommandFailed);
            }
            return Ok(Self { path });
        }
        Err(PublicationGatewayError::CommandFailed)
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn remove(self) -> Result<(), PublicationGatewayError> {
        fs::remove_file(&self.path).map_err(|_| PublicationGatewayError::CommandFailed)
    }
}

impl Drop for TemporaryPayloadFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

fn parse_api_pull_request(json: &str) -> Result<DraftPullRequest, PublicationGatewayError> {
    let value: Value =
        serde_json::from_str(json).map_err(|_| PublicationGatewayError::InvalidResponse)?;
    Ok(DraftPullRequest::new(
        value
            .get("number")
            .and_then(Value::as_u64)
            .ok_or(PublicationGatewayError::InvalidResponse)?,
        value
            .get("html_url")
            .and_then(Value::as_str)
            .ok_or(PublicationGatewayError::InvalidResponse)?,
        value
            .get("draft")
            .and_then(Value::as_bool)
            .ok_or(PublicationGatewayError::InvalidResponse)?,
        value
            .get("head")
            .and_then(|head| head.get("sha"))
            .and_then(Value::as_str)
            .ok_or(PublicationGatewayError::InvalidResponse)?,
        value
            .get("head")
            .and_then(|head| head.get("ref"))
            .and_then(Value::as_str)
            .ok_or(PublicationGatewayError::InvalidResponse)?,
        value
            .get("base")
            .and_then(|base| base.get("ref"))
            .and_then(Value::as_str)
            .ok_or(PublicationGatewayError::InvalidResponse)?,
    ))
}

fn verify_pull_request_content(
    value: &Value,
    payload: &ArtifactPublicationPayload,
) -> Result<(), PublicationGatewayError> {
    if value.get("title").and_then(Value::as_str) != Some(payload.title())
        || value.get("body").and_then(Value::as_str) != Some(payload.body())
    {
        return Err(PublicationGatewayError::ExistingPullRequestMismatch);
    }
    Ok(())
}

fn parse_pull_request_value(value: &Value) -> Result<DraftPullRequest, PublicationGatewayError> {
    Ok(DraftPullRequest::new(
        value
            .get("number")
            .and_then(Value::as_u64)
            .ok_or(PublicationGatewayError::InvalidResponse)?,
        value
            .get("url")
            .and_then(Value::as_str)
            .ok_or(PublicationGatewayError::InvalidResponse)?,
        value
            .get("isDraft")
            .and_then(Value::as_bool)
            .ok_or(PublicationGatewayError::InvalidResponse)?,
        value
            .get("headRefOid")
            .and_then(Value::as_str)
            .ok_or(PublicationGatewayError::InvalidResponse)?,
        value
            .get("headRefName")
            .and_then(Value::as_str)
            .ok_or(PublicationGatewayError::InvalidResponse)?,
        value
            .get("baseRefName")
            .and_then(Value::as_str)
            .ok_or(PublicationGatewayError::InvalidResponse)?,
    ))
}

fn verify_pull_request(
    pull_request: DraftPullRequest,
    payload: &ArtifactPublicationPayload,
    commit_sha: &str,
) -> Result<DraftPullRequest, PublicationGatewayError> {
    if pull_request.number() == 0
        || !pull_request.is_draft()
        || pull_request.head_sha() != commit_sha
        || pull_request.head_branch() != payload.head_branch()
        || pull_request.base_branch() != payload.base_branch()
        || !pull_request.url().starts_with("https://")
    {
        return Err(PublicationGatewayError::ExistingPullRequestMismatch);
    }
    Ok(pull_request)
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    #[test]
    fn api_pull_request_response_requires_exact_draft_identity() {
        let response = r#"{
            "number": 42,
            "html_url": "https://github.example/o/r/pull/42",
            "draft": true,
            "head": {"sha": "0123456789012345678901234567890123456789", "ref": "feature"},
            "base": {"ref": "main"}
        }"#;
        let pull_request = parse_api_pull_request(response).expect("REST response parses");
        assert_eq!(pull_request.number(), 42);
        assert!(pull_request.is_draft());
        assert_eq!(pull_request.head_branch(), "feature");
        assert_eq!(pull_request.base_branch(), "main");
    }

    #[test]
    fn publication_payload_file_is_private_and_removed_explicitly() {
        let file = TemporaryPayloadFile::create(&json!({
            "title": "sensitive title",
            "body": "sensitive body"
        }))
        .expect("private temporary file is created");
        let path = file.path().to_owned();
        let mode = fs::metadata(&path)
            .expect("file metadata is available")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
        let contents = fs::read_to_string(&path).expect("payload was written");
        assert!(contents.contains("sensitive title"));
        assert!(contents.contains("sensitive body"));
        file.remove().expect("temporary file is removed");
        assert!(!path.exists());
    }
}
