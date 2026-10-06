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
/// Git subprocesses used by an implementation must set `GIT_NO_REPLACE_OBJECTS=1`, so a
/// repository's replace refs cannot change which bytes the scanner sees for the supplied OID.
/// Implementations should return only a typed failure; diagnostics may contain secret material.
pub trait SecretScanner: Send + Sync {
    /// Returns `text` with every known secret replaced. Implementations must be
    /// deterministic and idempotent (`redact_text(redact_text(text))` equals
    /// `redact_text(text)`) and fail if complete redaction cannot be guaranteed.
    /// The default is fail-closed for callers that expose text; callers verify
    /// the returned value is a fixed point before persisting or returning it.
    fn redact_text(&self, _text: &str) -> Result<String, SecretScanError> {
        Err(SecretScanError::Unavailable)
    }

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
    /// Whether publication title/body bytes can be sent before side effects using a
    /// cancellable stdin implementation. Implementations must opt in when their process path
    /// can safely send the payload; the default fails closed. The returned capability must
    /// remain stable for the lifetime of the service using this gateway. The service checks it
    /// both before accepting a publication and before execution; changing it after acceptance
    /// can leave the accepted publication unable to run until startup recovery records it.
    fn supports_sensitive_stdin_payload(&self) -> bool {
        false
    }

    fn commit_tree(
        &self,
        repository: &Path,
        tree_oid: &str,
        base_commit: &str,
        message: &str,
        timeout: Duration,
    ) -> Result<String, PublicationGatewayError>;

    /// Verifies a commit's raw tree and sole parent headers using the same Git executable
    /// that created it. Implementations that cannot provide this check fail closed.
    fn verify_commit_tree_and_base(
        &self,
        _repository: &Path,
        _commit_sha: &str,
        _tree_oid: &str,
        _base_commit: &str,
        _timeout: Duration,
    ) -> bool {
        false
    }

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
    /// The operation was rejected before its corresponding remote mutation began.
    RejectedBeforeEffect,
    /// A read-only observation failed, so an existing remote result cannot be ruled out.
    ObservationUnavailableBeforeEffect,
    Spawn,
    CommandFailed,
    InvalidResponse,
    ExistingPullRequestMismatch,
}

impl fmt::Display for PublicationGatewayError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::RejectedBeforeEffect => "publication was rejected before remote effects",
            Self::ObservationUnavailableBeforeEffect => {
                "publication state could not be observed before remote effects"
            }
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
        request = request.env("GIT_NO_REPLACE_OBJECTS", "1");
        let output = ProcessRunner.run_git(request).map_err(map_process_error)?;
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
        let request = self.gh_request(repository, args, timeout, stdin_bytes);
        let output = ProcessRunner
            .run_with_env_removed(request, &["GH_REPO", "GH_HOST"])
            .map_err(map_process_error)?;
        if output.output_truncated {
            return Err(PublicationGatewayError::InvalidResponse);
        }
        String::from_utf8(output.stdout)
            .map(|value| value.trim().to_owned())
            .map_err(|_| PublicationGatewayError::InvalidResponse)
    }

    fn gh_request(
        &self,
        repository: &Path,
        args: &[&str],
        timeout: Duration,
        stdin_bytes: Option<Vec<u8>>,
    ) -> ProcessRequest {
        let mut request = ProcessRequest::new(self.gh_executable.clone())
            .cwd(repository)
            .timeout(timeout);
        request.args = args.iter().map(std::ffi::OsString::from).collect();
        request.stdin_bytes = stdin_bytes;
        request
    }
}

impl ArtifactPublicationGateway for GitHubArtifactPublicationGateway {
    fn supports_sensitive_stdin_payload(&self) -> bool {
        cfg!(unix)
    }

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

    fn verify_commit_tree_and_base(
        &self,
        repository: &Path,
        commit_sha: &str,
        tree_oid: &str,
        base_commit: &str,
        timeout: Duration,
    ) -> bool {
        if !valid_git_oid(commit_sha) {
            return false;
        }
        let mut request = ProcessRequest::new(self.git_executable.clone())
            .arg("-C")
            .arg(repository.as_os_str().to_owned())
            .args(["cat-file", "commit", commit_sha])
            .timeout(timeout);
        request.env.push((
            std::ffi::OsString::from("GIT_NO_REPLACE_OBJECTS"),
            std::ffi::OsString::from("1"),
        ));
        let output = match ProcessRunner.run_git(request) {
            Ok(output) if !output.output_truncated => output,
            _ => return false,
        };
        let Ok(text) = String::from_utf8(output.stdout) else {
            return false;
        };
        let Some(headers) = text.split_once("\n\n").map(|(headers, _)| headers) else {
            return false;
        };
        let mut tree = None;
        let mut parents = Vec::new();
        for line in headers.lines() {
            if let Some(value) = line.strip_prefix("tree ") {
                if tree.replace(value).is_some() {
                    return false;
                }
            } else if let Some(value) = line.strip_prefix("parent ") {
                parents.push(value);
            }
        }
        tree == Some(tree_oid) && parents.as_slice() == [base_commit]
    }

    fn push_commit(
        &self,
        repository: &Path,
        commit_sha: &str,
        head_branch: &str,
        timeout: Duration,
    ) -> Result<(), PublicationGatewayError> {
        // `git push origin` follows pushurl entries, which can target repositories
        // different from the fetch URL used below to select the Pull Request target.
        // Resolve and validate every effective push destination before any push effect.
        let fetch_url = self
            .git_output(repository, &["remote", "get-url", "origin"], timeout)
            .map_err(|_| PublicationGatewayError::RejectedBeforeEffect)?;
        let push_urls = self
            .git_output(
                repository,
                &["remote", "get-url", "--push", "--all", "origin"],
                timeout,
            )
            .map_err(|_| PublicationGatewayError::RejectedBeforeEffect)?;
        let expected = github_repository_from_remote_url(&fetch_url)
            .ok_or(PublicationGatewayError::RejectedBeforeEffect)?;
        validate_push_destinations(&expected, &push_urls)
            .map_err(|_| PublicationGatewayError::RejectedBeforeEffect)?;
        let refspec = format!("{commit_sha}:refs/heads/{head_branch}");
        self.git_output(
            repository,
            &["push", "--porcelain", "origin", &refspec],
            timeout,
        )
        .map_err(|error| match error {
            PublicationGatewayError::Spawn => PublicationGatewayError::RejectedBeforeEffect,
            other => other,
        })?;
        Ok(())
    }

    fn create_or_find_draft_pull_request(
        &self,
        repository: &Path,
        payload: &ArtifactPublicationPayload,
        commit_sha: &str,
        timeout: Duration,
    ) -> Result<DraftPullRequest, PublicationGatewayError> {
        let remote = self
            .git_output(repository, &["remote", "get-url", "origin"], timeout)
            .map_err(|_| PublicationGatewayError::ObservationUnavailableBeforeEffect)?;
        let remote = github_repository_from_remote_url(&remote)
            .ok_or(PublicationGatewayError::RejectedBeforeEffect)?;
        let listed = self
            .gh_output(
                repository,
                &[
                    "pr",
                    "list",
                    "--repo",
                    &remote.cli_selector,
                    "--state",
                    "all",
                    "--head",
                    payload.head_branch(),
                    "--base",
                    payload.base_branch(),
                    "--json",
                    "number,url,isDraft,headRefOid,headRefName,baseRefName,title,body,state",
                    "--limit",
                    "2",
                ],
                timeout,
            )
            .map_err(|_| PublicationGatewayError::ObservationUnavailableBeforeEffect)?;
        let values: Vec<Value> = serde_json::from_str(&listed)
            .map_err(|_| PublicationGatewayError::ObservationUnavailableBeforeEffect)?;
        if values.len() > 1 {
            return Err(PublicationGatewayError::RejectedBeforeEffect);
        }
        if let Some(value) = values.first() {
            match value.get("state").and_then(Value::as_str) {
                Some("OPEN") => {}
                Some("CLOSED" | "MERGED") => {
                    return Err(PublicationGatewayError::RejectedBeforeEffect);
                }
                Some(_) => {
                    return Err(PublicationGatewayError::ObservationUnavailableBeforeEffect);
                }
                None => {
                    return Err(PublicationGatewayError::ObservationUnavailableBeforeEffect);
                }
            }
            let existing = parse_pull_request_value(value)
                .map_err(|_| PublicationGatewayError::ObservationUnavailableBeforeEffect)?;
            verify_pull_request_content(value, payload)
                .map_err(|_| PublicationGatewayError::RejectedBeforeEffect)?;
            return verify_pull_request(existing, payload, commit_sha)
                .map_err(|_| PublicationGatewayError::RejectedBeforeEffect);
        }

        let request_body = json!({
            "base": payload.base_branch(),
            "head": payload.head_branch(),
            "title": payload.title(),
            "body": payload.body(),
            "draft": true,
        });
        let request_body = serde_json::to_vec(&request_body)
            .map_err(|_| PublicationGatewayError::RejectedBeforeEffect)?;
        let endpoint = format!("repos/{}/pulls", remote.api_path);
        let created = self.gh_output_with_stdin(
            repository,
            &[
                "api",
                "--hostname",
                &remote.hostname,
                &endpoint,
                "--method",
                "POST",
                "--input",
                "-",
            ],
            timeout,
            Some(request_body),
        );
        let created = created.map_err(|error| match error {
            PublicationGatewayError::Spawn => PublicationGatewayError::RejectedBeforeEffect,
            other => other,
        })?;
        let created_value: Value =
            serde_json::from_str(&created).map_err(|_| PublicationGatewayError::InvalidResponse)?;
        verify_pull_request_content(&created_value, payload)?;
        verify_pull_request(parse_api_pull_request(&created)?, payload, commit_sha)
    }
}

fn validate_push_destinations(
    expected: &GitHubRemoteRepository,
    push_urls: &str,
) -> Result<(), PublicationGatewayError> {
    let mut destinations = push_urls
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty());
    let first = destinations
        .next()
        .ok_or(PublicationGatewayError::InvalidResponse)?;
    for destination in std::iter::once(first).chain(destinations) {
        let parsed = github_repository_from_remote_url(destination)
            .ok_or(PublicationGatewayError::InvalidResponse)?;
        if parsed != *expected {
            return Err(PublicationGatewayError::InvalidResponse);
        }
    }
    Ok(())
}

fn map_process_error(error: crate::process_runner::ProcessError) -> PublicationGatewayError {
    match error {
        crate::process_runner::ProcessError::Spawn(_) => PublicationGatewayError::Spawn,
        _ => PublicationGatewayError::CommandFailed,
    }
}

fn valid_git_oid(value: &str) -> bool {
    matches!(value.len(), 40 | 64) && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[derive(Debug, Eq, PartialEq)]
struct GitHubRemoteRepository {
    hostname: String,
    cli_selector: String,
    api_path: String,
}

fn github_repository_from_remote_url(remote: &str) -> Option<GitHubRemoteRepository> {
    let remote = remote.trim();
    let (host, path) = if let Some((scheme, remainder)) = remote.split_once("://") {
        if !matches!(scheme, "https" | "ssh") {
            return None;
        }
        let (authority, path) = remainder.split_once('/')?;
        (authority.rsplit('@').next()?, path)
    } else {
        let (authority, path) = remote.rsplit_once(':')?;
        if path.starts_with('/') || !authority.contains('@') {
            return None;
        }
        (authority.rsplit('@').next()?, path)
    };
    if host.is_empty()
        || !host
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-'))
    {
        return None;
    }
    let path = path.trim_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    let mut components = path.split('/');
    let owner = components.next()?;
    let name = components.next()?;
    if components.next().is_some()
        || !valid_repository_component(owner)
        || !valid_repository_component(name)
    {
        return None;
    }
    let api_path = format!("{owner}/{name}");
    let host = host.to_ascii_lowercase();
    let cli_selector = if host == "github.com" {
        api_path.clone()
    } else {
        format!("{host}/{api_path}")
    };
    Some(GitHubRemoteRepository {
        hostname: host,
        cli_selector,
        api_path,
    })
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
    if value.get("state").and_then(Value::as_str) != Some("open") {
        return Err(PublicationGatewayError::InvalidResponse);
    }
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
    let title_matches = value.get("title").and_then(Value::as_str) == Some(payload.title());
    let body_matches = match value.get("body") {
        Some(Value::String(body)) => body == payload.body(),
        Some(Value::Null) => payload.body().is_empty(),
        _ => false,
    };
    if !title_matches || !body_matches {
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
    use super::*;

    #[test]
    fn origin_remote_is_normalized_to_explicit_github_repository_selectors() {
        for remote in [
            "https://github.com/example/project.git",
            "git@github.com:example/project.git",
            "ssh://git@github.com/example/project.git",
        ] {
            assert_eq!(
                github_repository_from_remote_url(remote),
                Some(GitHubRemoteRepository {
                    hostname: "github.com".into(),
                    cli_selector: "example/project".into(),
                    api_path: "example/project".into(),
                })
            );
        }
        assert_eq!(
            github_repository_from_remote_url("ssh://git@github.example/example/project.git"),
            Some(GitHubRemoteRepository {
                hostname: "github.example".into(),
                cli_selector: "github.example/example/project".into(),
                api_path: "example/project".into(),
            })
        );
        assert!(github_repository_from_remote_url("not-a-github-remote").is_none());
        assert!(
            github_repository_from_remote_url("http://github.com/example/project.git").is_none()
        );
        assert!(
            github_repository_from_remote_url("git://github.com/example/project.git").is_none()
        );
    }

    #[test]
    fn push_destinations_must_match_origin_repository() {
        let expected = github_repository_from_remote_url("https://github.com/example/project.git")
            .expect("valid origin URL");

        // With no configured pushurl, Git reports the fetch URL as the effective destination.
        validate_push_destinations(&expected, "https://github.com/example/project.git\n")
            .expect("unset pushurl falls back to origin");
        validate_push_destinations(&expected, "git@github.com:example/project.git\n")
            .expect("equivalent SSH URL targets same repository");
        validate_push_destinations(
            &expected,
            "https://github.com/example/project.git\ngit@github.com:example/project.git\n",
        )
        .expect("all configured pushurls target same repository");
        assert_eq!(
            validate_push_destinations(&expected, "https://github.com/other/project.git\n"),
            Err(PublicationGatewayError::InvalidResponse)
        );
        assert_eq!(
            validate_push_destinations(
                &expected,
                "https://github.com/example/project.git\nhttps://github.com/other/project.git\n",
            ),
            Err(PublicationGatewayError::InvalidResponse)
        );
    }

    #[cfg(unix)]
    #[test]
    fn mismatched_pushurl_is_rejected_before_git_push_is_invoked() {
        use std::{
            os::unix::fs::PermissionsExt,
            time::{SystemTime, UNIX_EPOCH},
        };

        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock is after epoch")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "artifact-pushurl-gateway-{}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).expect("create fake repository directory");
        let repository = root.join("repository");
        std::fs::create_dir(&repository).expect("create repository");
        let invocations = root.join("invocations");
        let fake_git = root.join("git");
        std::fs::write(
            &fake_git,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$3:$5\" >> '{}'\ncase \"$3:$5\" in\n  remote:origin) printf '%s\\n' 'https://github.com/example/project.git' ;;\n  remote:--push) printf '%s\\n' 'https://github.com/other/project.git' ;;\n  push:*) printf '%s\\n' 'push' >> '{}' ;;\n  *) exit 90 ;;\nesac\n",
                invocations.display(),
                invocations.display(),
            ),
        )
        .expect("write fake git executable");
        let mut permissions = std::fs::metadata(&fake_git)
            .expect("stat fake git executable")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&fake_git, permissions).expect("make fake git executable");

        let gateway = GitHubArtifactPublicationGateway::with_executables(&fake_git, "gh");
        assert_eq!(
            gateway.push_commit(
                &repository,
                "0123456789012345678901234567890123456789",
                "feature",
                Duration::from_secs(5),
            ),
            Err(PublicationGatewayError::RejectedBeforeEffect)
        );
        let calls = std::fs::read_to_string(&invocations).expect("read fake git invocations");
        assert!(calls.contains("remote:origin"));
        assert!(calls.contains("remote:--push"));
        assert!(
            !calls.lines().any(|call| call.starts_with("push:")),
            "git push must not be invoked: {calls}"
        );
        std::fs::remove_dir_all(root).expect("remove fake repository");
    }

    #[cfg(unix)]
    #[test]
    fn pull_request_list_failure_is_observation_unavailable_before_creation() {
        use std::{
            os::unix::fs::PermissionsExt,
            time::{SystemTime, UNIX_EPOCH},
        };

        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock is after epoch")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "artifact-pr-list-gateway-{}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).expect("create fake gateway directory");
        let repository = root.join("repository");
        std::fs::create_dir(&repository).expect("create repository");
        let calls = root.join("gh-calls");
        let fake_git = root.join("git");
        std::fs::write(
            &fake_git,
            "#!/bin/sh\ncase \"$3:$4\" in\n  remote:get-url) printf '%s\\n' 'https://github.com/example/project.git' ;;\n  *) exit 90 ;;\nesac\n",
        )
        .expect("write fake git executable");
        let fake_gh = root.join("gh");
        std::fs::write(
            &fake_gh,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\nexit 1\n",
                calls.display()
            ),
        )
        .expect("write fake gh executable");
        for executable in [&fake_git, &fake_gh] {
            let mut permissions = std::fs::metadata(executable)
                .expect("stat fake executable")
                .permissions();
            permissions.set_mode(0o755);
            std::fs::set_permissions(executable, permissions)
                .expect("make fake executable executable");
        }

        let gateway = GitHubArtifactPublicationGateway::with_executables(&fake_git, &fake_gh);
        assert_eq!(
            gateway.create_or_find_draft_pull_request(
                &repository,
                &ArtifactPublicationPayload::new("main", "feature", "title", "body"),
                "0123456789012345678901234567890123456789",
                Duration::from_secs(2),
            ),
            Err(PublicationGatewayError::ObservationUnavailableBeforeEffect)
        );
        let invocations = std::fs::read_to_string(&calls).expect("read fake gh calls");
        assert!(invocations.contains("pr list"));
        assert!(!invocations.contains("api"));
        std::fs::remove_dir_all(root).expect("remove fake gateway directory");
    }

    #[cfg(unix)]
    #[test]
    fn existing_pull_request_is_reused_only_when_open() {
        use std::{
            os::unix::fs::PermissionsExt,
            time::{SystemTime, UNIX_EPOCH},
        };

        for (state, expected) in [
            ("OPEN", Ok(())),
            ("CLOSED", Err(PublicationGatewayError::RejectedBeforeEffect)),
            (
                "UNKNOWN_FUTURE_STATE",
                Err(PublicationGatewayError::ObservationUnavailableBeforeEffect),
            ),
        ] {
            let unique = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock is after epoch")
                .as_nanos();
            let root = std::env::temp_dir().join(format!(
                "artifact-existing-pr-gateway-{}-{unique}",
                std::process::id()
            ));
            std::fs::create_dir_all(&root).expect("create fake gateway directory");
            let repository = root.join("repository");
            std::fs::create_dir(&repository).expect("create repository");
            let calls = root.join("gh-calls");
            let fake_git = root.join("git");
            std::fs::write(
                &fake_git,
                "#!/bin/sh\ncase \"$3:$4\" in\n  remote:get-url) printf '%s\\n' 'https://github.com/example/project.git' ;;\n  *) exit 90 ;;\nesac\n",
            )
            .expect("write fake git executable");
            let fake_gh = root.join("gh");
            std::fs::write(
                &fake_gh,
                format!(
                    "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\nprintf '%s\\n' '[{{\"number\":17,\"url\":\"https://github.com/example/project/pull/17\",\"isDraft\":true,\"headRefOid\":\"0123456789012345678901234567890123456789\",\"headRefName\":\"feature\",\"baseRefName\":\"main\",\"title\":\"title\",\"body\":\"body\",\"state\":\"{state}\"}}]'\n",
                    calls.display()
                ),
            )
            .expect("write fake gh executable");
            for executable in [&fake_git, &fake_gh] {
                let mut permissions = std::fs::metadata(executable)
                    .expect("stat fake executable")
                    .permissions();
                permissions.set_mode(0o755);
                std::fs::set_permissions(executable, permissions)
                    .expect("make fake executable executable");
            }

            let gateway = GitHubArtifactPublicationGateway::with_executables(&fake_git, &fake_gh);
            let result = gateway.create_or_find_draft_pull_request(
                &repository,
                &ArtifactPublicationPayload::new("main", "feature", "title", "body"),
                "0123456789012345678901234567890123456789",
                Duration::from_secs(2),
            );
            match expected {
                Ok(()) => assert!(
                    result.is_ok(),
                    "open matching PR should be reused: {result:?}"
                ),
                Err(error) => assert_eq!(result, Err(error)),
            }
            let invocations = std::fs::read_to_string(&calls).expect("read fake gh calls");
            assert!(invocations.contains("headRefName,baseRefName,title,body,state"));
            assert!(!invocations.contains("api"));
            std::fs::remove_dir_all(root).expect("remove fake gateway directory");
        }
    }

    #[cfg(unix)]
    #[test]
    fn commit_verification_uses_configured_git_and_raw_headers_without_replacements() {
        use std::{
            os::unix::fs::PermissionsExt,
            time::{SystemTime, UNIX_EPOCH},
        };

        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock is after epoch")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "artifact-commit-verify-{}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).expect("create fake repository directory");
        let repository = root.join("repository");
        std::fs::create_dir(&repository).expect("create repository");
        let invocations = root.join("invocations");
        let fake_git = root.join("custom-git");
        let tree_oid = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let base_commit = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        let commit_sha = "cccccccccccccccccccccccccccccccccccccccc";
        std::fs::write(
            &fake_git,
            format!(
                "#!/bin/sh\nprintf '%s:%s:%s:%s\\n' \"${{GIT_NO_REPLACE_OBJECTS-unset}}\" \"$3\" \"$4\" \"$5\" > '{}'\nprintf '%s\\n' 'tree {tree_oid}' 'parent {base_commit}' 'author Test <test@example.invalid> 0 +0000' '' 'message'\n",
                invocations.display(),
            ),
        )
        .expect("write custom git executable");
        let mut permissions = std::fs::metadata(&fake_git)
            .expect("stat custom git executable")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&fake_git, permissions).expect("make custom git executable");

        let gateway = GitHubArtifactPublicationGateway::with_executables(&fake_git, "gh");
        assert!(gateway.verify_commit_tree_and_base(
            &repository,
            commit_sha,
            tree_oid,
            base_commit,
            Duration::from_secs(2),
        ));
        assert_eq!(
            std::fs::read_to_string(&invocations).expect("read custom git invocation"),
            format!("1:cat-file:commit:{commit_sha}\n")
        );
        std::fs::remove_dir_all(root).expect("remove fake repository");
    }

    #[test]
    fn api_pull_request_response_requires_exact_draft_identity() {
        let response = r#"{
            "number": 42,
            "html_url": "https://github.example/o/r/pull/42",
            "state": "open",
            "draft": true,
            "head": {"sha": "0123456789012345678901234567890123456789", "ref": "feature"},
            "base": {"ref": "main"}
        }"#;
        let pull_request = parse_api_pull_request(response).expect("REST response parses");
        assert_eq!(pull_request.number(), 42);
        assert!(pull_request.is_draft());
        assert_eq!(pull_request.head_branch(), "feature");
        assert_eq!(pull_request.base_branch(), "main");
        assert_eq!(
            parse_api_pull_request(&response.replace("\"open\"", "\"closed\"")),
            Err(PublicationGatewayError::InvalidResponse)
        );
    }

    #[test]
    fn pull_request_null_body_matches_only_an_empty_requested_body() {
        let empty_payload = ArtifactPublicationPayload::new("main", "feature", "title", "");
        verify_pull_request_content(&json!({"title": "title", "body": null}), &empty_payload)
            .expect("GitHub null body represents an empty body");

        let nonempty_payload = ArtifactPublicationPayload::new("main", "feature", "title", "body");
        assert_eq!(
            verify_pull_request_content(
                &json!({"title": "title", "body": null}),
                &nonempty_payload,
            ),
            Err(PublicationGatewayError::ExistingPullRequestMismatch)
        );
    }

    #[test]
    fn pull_request_non_string_body_is_rejected() {
        let payload = ArtifactPublicationPayload::new("main", "feature", "title", "");
        assert_eq!(
            verify_pull_request_content(&json!({"title": "title", "body": 42}), &payload),
            Err(PublicationGatewayError::ExistingPullRequestMismatch)
        );
    }

    #[cfg(unix)]
    #[test]
    fn gh_api_publication_payload_is_stdin_only() {
        let gateway = GitHubArtifactPublicationGateway::with_executables("git", "fake-gh");
        let body = b"private title and body".to_vec();
        let args = [
            "api",
            "--hostname",
            "github.com",
            "repos/example/project/pulls",
            "--method",
            "POST",
            "--input",
            "-",
        ];
        let request = gateway.gh_request(
            Path::new("."),
            &args,
            Duration::from_secs(2),
            Some(body.clone()),
        );
        let request_args = request
            .args
            .iter()
            .map(|argument| argument.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert_eq!(request_args, args);
        assert_eq!(request.stdin_bytes.as_deref(), Some(body.as_slice()));
        assert!(!request_args.join(" ").contains("private title and body"));

        let fixture = ProcessRequest::new("sh")
            .args([
                "-c",
                "printf 'repo=%s host=%s argv=%s\n' \"${GH_REPO-unset}\" \"${GH_HOST-unset}\" \"$*\"; count=$(wc -c | tr -d ' '); printf 'stdin-bytes=%s\n' \"$count\"",
                "fake-gh",
            ])
            .args(request.args.clone())
            .stdin_bytes(body.clone())
            .timeout(Duration::from_secs(2))
            .test_inherited_env("GH_REPO", "wrong/target")
            .test_inherited_env("GH_HOST", "wrong.host");
        let result = ProcessRunner
            .run_with_env_removed(fixture, &["GH_REPO", "GH_HOST"])
            .expect("shell fixture consumes stdin");
        let result = String::from_utf8(result.stdout).expect("fixture output is UTF-8");
        assert!(result.contains("repo=unset host=unset"));
        assert!(result.contains("--hostname github.com repos/example/project/pulls"));
        assert!(result.contains("--input -"));
        assert!(
            result.contains(&format!("stdin-bytes={}", body.len())),
            "unexpected fake CLI output: {result:?}"
        );
        assert!(!result.contains("private title and body"));
    }
}
