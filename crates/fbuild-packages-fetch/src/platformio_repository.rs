//! Lock GitHub-hosted PlatformIO repository packages to immutable commits.

use fbuild_config::PackageOverride;
use fbuild_core::platformio_package::{PackageLock, PackageSource};
use sha2::{Digest, Sha256};
use std::time::Duration;
use tokio::io::AsyncReadExt;

#[derive(Debug, thiserror::Error)]
pub enum RepositoryError {
    #[error("unsupported PlatformIO repository URL: {0}")]
    UnsupportedUrl(String),
    #[error("invalid PlatformIO repository ref: {0}")]
    InvalidRef(String),
    #[error("PlatformIO repository Git command failed: {0}")]
    Git(String),
    #[error("PlatformIO repository returned an invalid commit: {0}")]
    InvalidCommit(String),
    #[error("PlatformIO archive resolution failed: {0}")]
    Archive(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedRepository {
    pub lock: PackageLock,
    pub archive: PackageOverride,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedArchive {
    pub lock: PackageLock,
    pub archive: PackageOverride,
}

/// Fetch a mutable archive URL to establish its content identity before it
/// becomes a package-cache key. The later staged install verifies this digest,
/// so a server changing between resolution and installation fails closed.
pub async fn resolve_archive_source(
    url: &str,
    revision: Option<&str>,
) -> Result<ResolvedArchive, RepositoryError> {
    let temp =
        tempfile::TempDir::new().map_err(|error| RepositoryError::Archive(error.to_string()))?;
    let path = crate::downloader::download_file(url, temp.path())
        .await
        .map_err(|error| RepositoryError::Archive(error.to_string()))?;
    let mut file = tokio::fs::File::open(path)
        .await
        .map_err(|error| RepositoryError::Archive(error.to_string()))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .await
            .map_err(|error| RepositoryError::Archive(error.to_string()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    let sha256 = format!("{:x}", hasher.finalize());
    Ok(ResolvedArchive {
        lock: PackageLock::Archive {
            url: url.into(),
            sha256: sha256.clone(),
        },
        archive: PackageOverride {
            url: url.into(),
            version: revision.unwrap_or("archive").into(),
            checksum: Some(sha256),
        },
    })
}

/// Resolve a GitHub repository source to an immutable commit. Other VCS hosts
/// fail explicitly until a transport for them exists; they never become a
/// default fbuild package. The archive URL is keyed by the resolved SHA.
pub async fn resolve_github_repository(
    source: &PackageSource,
) -> Result<ResolvedRepository, RepositoryError> {
    let PackageSource::Repository { url, reference } = source else {
        return Err(RepositoryError::UnsupportedUrl(format!("{source:?}")));
    };
    let (owner, repo) = github_owner_repo(url)?;
    let reference = reference.as_deref().unwrap_or("HEAD");
    if reference.is_empty() || reference.len() > 1024 || reference.contains(['\n', '\r', '\0']) {
        return Err(RepositoryError::InvalidRef(reference.into()));
    }
    let commit = if is_full_sha(reference) {
        reference.to_ascii_lowercase()
    } else {
        let candidates = candidate_refs(reference);
        // allow-direct-spawn: PlatformIO VCS package ref lookup, no shell.
        let mut command = tokio::process::Command::new("git");
        command
            .args(["ls-remote", url])
            .args(&candidates)
            .kill_on_drop(true);
        let output = tokio::time::timeout(Duration::from_secs(30), command.output())
            .await
            .map_err(|_| RepositoryError::Git(format!("timed out resolving {reference}")))?
            .map_err(|error| RepositoryError::Git(error.to_string()))?;
        if !output.status.success() {
            return Err(RepositoryError::Git(
                String::from_utf8_lossy(&output.stderr).trim().to_string(),
            ));
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        let commit = select_remote_commit(&stdout, &candidates)
            .ok_or_else(|| RepositoryError::InvalidRef(reference.into()))?;
        if !is_full_sha(commit) {
            return Err(RepositoryError::InvalidCommit(commit.into()));
        }
        commit.to_ascii_lowercase()
    };
    Ok(github_archive(url, &owner, &repo, &commit))
}

fn candidate_refs(reference: &str) -> Vec<String> {
    if reference == "HEAD" {
        return vec!["HEAD".into()];
    }
    if reference.starts_with("refs/tags/") {
        return vec![reference.into(), format!("{reference}^{{}}")];
    }
    if reference.starts_with("refs/") {
        return vec![reference.into()];
    }
    vec![
        format!("refs/heads/{reference}"),
        format!("refs/tags/{reference}"),
        format!("refs/tags/{reference}^{{}}"),
    ]
}

fn select_remote_commit<'a>(stdout: &'a str, candidates: &[String]) -> Option<&'a str> {
    let refs = stdout
        .lines()
        .filter_map(|line| line.split_once('\t'))
        .map(|(sha, name)| (name, sha))
        .collect::<std::collections::HashMap<_, _>>();
    // Branch names win if a remote has both a branch and a tag; an annotated
    // tag must use its peeled commit, not the tag object's SHA.
    candidates
        .iter()
        .filter(|name| !name.starts_with("refs/tags/") || name.ends_with("^{}"))
        .chain(
            candidates
                .iter()
                .filter(|name| name.starts_with("refs/tags/") && !name.ends_with("^{}")),
        )
        .find_map(|name| refs.get(name.as_str()).copied())
}

fn github_owner_repo(url: &str) -> Result<(String, String), RepositoryError> {
    let parsed =
        reqwest::Url::parse(url).map_err(|_| RepositoryError::UnsupportedUrl(url.into()))?;
    if parsed.scheme() != "https"
        || parsed.host_str() != Some("github.com")
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
    {
        return Err(RepositoryError::UnsupportedUrl(url.into()));
    }
    let segments: Vec<_> = parsed
        .path_segments()
        .ok_or_else(|| RepositoryError::UnsupportedUrl(url.into()))?
        .filter(|segment| !segment.is_empty())
        .collect();
    let [owner, repo] = segments.as_slice() else {
        return Err(RepositoryError::UnsupportedUrl(url.into()));
    };
    let repo = repo.strip_suffix(".git").unwrap_or(repo);
    if !valid_segment(owner) || !valid_segment(repo) {
        return Err(RepositoryError::UnsupportedUrl(url.into()));
    }
    Ok(((*owner).into(), repo.into()))
}

fn valid_segment(value: &str) -> bool {
    !value.is_empty()
        && value != "."
        && value != ".."
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

fn is_full_sha(value: &str) -> bool {
    value.len() == 40 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn github_archive(url: &str, owner: &str, repo: &str, commit: &str) -> ResolvedRepository {
    ResolvedRepository {
        lock: PackageLock::Repository {
            url: url.into(),
            commit: commit.into(),
        },
        archive: PackageOverride::new(
            format!("https://github.com/{owner}/{repo}/archive/{commit}.tar.gz"),
            commit,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fbuild_core::platformio_package::parse_package_spec;

    #[tokio::test]
    async fn archive_lock_changes_when_bytes_at_the_same_url_change() {
        let bytes = std::sync::Arc::new(tokio::sync::RwLock::new(b"first archive".to_vec()));
        let served = bytes.clone();
        let app = axum::Router::new().route(
            "/platform.tar.gz",
            axum::routing::get(move || {
                let served = served.clone();
                async move { axum::body::Bytes::from(served.read().await.clone()) }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/platform.tar.gz", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let first = resolve_archive_source(&url, Some("test-revision"))
            .await
            .unwrap();
        *bytes.write().await = b"second archive".to_vec();
        let second = resolve_archive_source(&url, Some("test-revision"))
            .await
            .unwrap();
        server.abort();

        assert_eq!(first.archive.url, second.archive.url);
        assert_eq!(first.archive.version, second.archive.version);
        assert_ne!(first.archive.checksum, second.archive.checksum);
        assert_ne!(first.lock.cache_identity(), second.lock.cache_identity());
    }

    #[tokio::test]
    async fn immutable_ch32v_platform_source_becomes_archive_lock() {
        let spec = parse_package_spec("https://github.com/Community-PIO-CH32V/platform-ch32v.git#b7397c29a71101175bfc94f6ab06f9daac336458").unwrap();
        let resolved = resolve_github_repository(&spec.source).await.unwrap();
        assert_eq!(
            resolved.archive.url,
            "https://github.com/Community-PIO-CH32V/platform-ch32v/archive/b7397c29a71101175bfc94f6ab06f9daac336458.tar.gz"
        );
        assert_eq!(
            resolved.archive.version,
            "b7397c29a71101175bfc94f6ab06f9daac336458"
        );
        assert_eq!(resolved.lock.cache_identity().len(), 64);
    }

    #[tokio::test]
    async fn unsupported_repository_host_fails_without_substitution() {
        let spec = parse_package_spec(
            "https://gitlab.com/example/ch32v.git#0123456789abcdef0123456789abcdef01234567",
        )
        .unwrap();
        assert!(matches!(
            resolve_github_repository(&spec.source).await,
            Err(RepositoryError::UnsupportedUrl(_))
        ));
    }

    #[test]
    fn annotated_tags_resolve_to_the_peeled_commit() {
        let candidates = candidate_refs("v1.1.0");
        let tag = "a".repeat(40);
        let commit = "b".repeat(40);
        let listing = format!("{tag}\trefs/tags/v1.1.0\n{commit}\trefs/tags/v1.1.0^{{}}\n");
        assert_eq!(
            select_remote_commit(&listing, &candidates),
            Some(commit.as_str())
        );
    }

    #[test]
    fn branch_takes_precedence_over_same_named_tag() {
        let candidates = candidate_refs("release");
        let branch = "c".repeat(40);
        let tag = "d".repeat(40);
        let listing = format!("{tag}\trefs/tags/release\n{branch}\trefs/heads/release\n");
        assert_eq!(
            select_remote_commit(&listing, &candidates),
            Some(branch.as_str())
        );
    }

    #[test]
    fn unavailable_ref_cannot_resolve_to_head_or_a_default_package() {
        let candidates = candidate_refs("missing-branch");
        let listing = format!("{}\tHEAD\n", "e".repeat(40));
        assert_eq!(select_remote_commit(&listing, &candidates), None);
    }
}
