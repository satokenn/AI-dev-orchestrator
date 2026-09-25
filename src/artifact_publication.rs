//! Fail-closed publication of an immutable Artifact tree as a GitHub Draft PR.

use std::{
    fmt,
    path::{Path, PathBuf},
    time::Duration,
};

use serde_json::{Value, json};

use crate::process_runner::{ProcessRequest, ProcessRunner};

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
        self.gh_output_with_stdin(repository, args, timeout, None)
    }

    fn gh_output_with_stdin(
        &self,
        repository: &Path,
        args: &[&str],
        timeout: Duration,
        stdin_bytes: Option<Vec<u8>>,
    ) -> Result<String, PublicationGatewayError> {
        let mut request = ProcessRequest::new(self.gh_executable.clone())
            .cwd(repository)
            .timeout(timeout);
        request.args = args.iter().map(std::ffi::OsString::from).collect();
        request.stdin_bytes = stdin_bytes;
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
        let request_body = serde_json::to_vec(&request_body)
            .map_err(|_| PublicationGatewayError::InvalidResponse)?;
        let endpoint = format!("repos/{repo}/pulls");
        let created = self.gh_output_with_stdin(
            repository,
            &["api", &endpoint, "--method", "POST", "--input", "-"],
            timeout,
            Some(request_body),
        );
        let created = created?;
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
    #[cfg(unix)]
    use std::{
        fs,
        os::unix::fs::PermissionsExt,
        sync::atomic::{AtomicU64, Ordering},
    };

    use super::*;

    #[cfg(unix)]
    static NEXT_TEST_DIR: AtomicU64 = AtomicU64::new(1);

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

    #[cfg(unix)]
    #[test]
    fn gh_api_publication_payload_is_stdin_only() {
        let directory = std::env::temp_dir().join(format!(
            "gh-api-stdin-test-{}-{}",
            std::process::id(),
            NEXT_TEST_DIR.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&directory).expect("isolated test directory is created");
        let executable = directory.join("fake-gh");
        fs::write(
            &executable,
            "#!/bin/sh\nprintf '%s\\n' \"$@\"\ncount=$(wc -c | tr -d ' ')\nprintf 'stdin-bytes=%s\\n' \"$count\"\n",
        )
        .expect("fake CLI is written");
        let mut permissions = fs::metadata(&executable)
            .expect("fake CLI metadata is available")
            .permissions();
        permissions.set_mode(0o700);
        fs::set_permissions(&executable, permissions).expect("fake CLI is executable");

        let gateway = GitHubArtifactPublicationGateway::with_executables("git", executable);
        let body = b"private title and body".to_vec();
        let result = gateway
            .gh_output_with_stdin(
                Path::new("."),
                &[
                    "api",
                    "repos/example/project/pulls",
                    "--method",
                    "POST",
                    "--input",
                    "-",
                ],
                Duration::from_secs(2),
                Some(body.clone()),
            )
            .expect("fake CLI consumes stdin");
        assert!(result.contains("--input\n-\n"));
        assert!(
            result.contains(&format!("stdin-bytes={}", body.len())),
            "unexpected fake CLI output: {result:?}"
        );
        assert!(!result.contains("private title and body"));
        fs::remove_dir_all(directory).expect("test files are removed");
    }
}
