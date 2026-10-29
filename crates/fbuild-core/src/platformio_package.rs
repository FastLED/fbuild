//! PlatformIO package specifications and registry payload metadata.
//!
//! This is deliberately independent of [`crate::Platform`]: resolving a
//! published package and knowing how to compile its board are different jobs.
//! Network access, downloads, and extraction belong to the package-fetch
//! layer; callers pass registry and platform-manifest JSON to these functions.

use semver::{Version, VersionReq};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::path::NormalizedPath;

const REGISTRY_API: &str = "https://api.registry.platformio.org/v3/packages";

/// PlatformIO registry `system` selector for a native fbuild host. Unknown
/// architectures fail resolution rather than selecting another host's binary.
#[must_use]
pub const fn host_system(host: crate::platform::host::HostPlatform) -> Option<&'static str> {
    use crate::platform::host::{HostArch, HostOs};
    match (host.os(), host.arch()) {
        (HostOs::Linux, HostArch::X86_64) => Some("linux_x86_64"),
        (HostOs::Linux, HostArch::Aarch64) => Some("linux_aarch64"),
        (HostOs::Linux, HostArch::X86) => Some("linux_i686"),
        (HostOs::Windows, HostArch::X86_64) => Some("windows_amd64"),
        (HostOs::Windows, HostArch::Aarch64) => Some("windows_arm64"),
        (HostOs::Windows, HostArch::X86) => Some("windows_x86"),
        (HostOs::Macos, HostArch::X86_64) => Some("darwin_x86_64"),
        (HostOs::Macos, HostArch::Aarch64) => Some("darwin_arm64"),
        _ => None,
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ResolutionError {
    #[error("invalid PlatformIO package specification: {0}")]
    InvalidSpec(String),
    #[error("registry owner is required to fetch metadata for {0}")]
    OwnerRequired(String),
    #[error("invalid PlatformIO registry metadata: {0}")]
    InvalidMetadata(String),
    #[error("no registry version of {package} matches {requirement}")]
    VersionNotFound {
        package: String,
        requirement: String,
    },
    #[error("no payload for {package}@{version} on {system}")]
    UnsupportedHost {
        package: String,
        version: String,
        system: String,
    },
    #[error("no platform package named {0} exists in the manifest")]
    MissingPackage(String),
    #[error("ambiguous registry alias {name}: {owners:?}")]
    AmbiguousAlias { name: String, owners: Vec<String> },
}

pub type Result<T> = std::result::Result<T, ResolutionError>;

/// The registry's package categories. Frameworks are distributed as `tool`
/// packages by PlatformIO, although they remain a distinct build-level kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PackageKind {
    Platform,
    Tool,
    Framework,
    Library,
}

impl PackageKind {
    #[must_use]
    pub fn registry_type(self) -> &'static str {
        match self {
            Self::Platform => "platform",
            Self::Tool | Self::Framework => "tool",
            Self::Library => "library",
        }
    }
}

/// A registry name and optional owner/version requirement. `owner = None`
/// means the package-fetch layer must discover the owner before requesting
/// `/packages/{owner}/{type}/{name}`; it must not guess `platformio`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegistrySpec {
    pub owner: Option<String>,
    pub name: String,
    pub requirement: Option<String>,
    /// Explicit PlatformIO registry category from an `owner/type/name` or
    /// `type/owner/name` path. Framework/toolchain aliases both use `tool`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub registry_type: Option<String>,
}

/// A source accepted by PlatformIO's package-spec grammar. An archive URL or
/// repository ref is not yet content-locked: fetching it establishes its
/// digest/commit. Registry payloads carry a published SHA-256 already.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PackageSource {
    Registry(RegistrySpec),
    Archive {
        url: String,
        /// Fragment used by existing PlatformIO overrides as a source/version
        /// discriminator; it is not an archive checksum.
        revision: Option<String>,
    },
    Repository {
        url: String,
        reference: Option<String>,
    },
    LocalPath {
        path: NormalizedPath,
    },
}

/// Parsed package request. `alias` is the package name before `@` in a
/// source override such as `framework-foo@https://host/core.tar.gz`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackageSpec {
    pub alias: Option<String>,
    pub source: PackageSource,
}

impl PackageSpec {
    #[must_use]
    pub fn registry(&self) -> Option<&RegistrySpec> {
        match &self.source {
            PackageSource::Registry(registry) => Some(registry),
            _ => None,
        }
    }

    #[must_use]
    pub fn package_name(&self) -> Option<&str> {
        self.alias
            .as_deref()
            .or_else(|| self.registry().map(|r| r.name.as_str()))
    }
}

/// The immutable file selected from a registry response for one host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedPayload {
    pub owner: String,
    pub kind: PackageKind,
    pub name: String,
    pub version: String,
    pub system: String,
    pub url: String,
    pub sha256: String,
}

impl ResolvedPayload {
    /// Cache identity includes both package identity and archive digest. A
    /// metadata update that changes the artifact cannot reuse an older cache.
    #[must_use]
    pub fn cache_identity(&self) -> String {
        let mut hash = Sha256::new();
        for field in [
            &self.owner,
            self.kind.registry_type(),
            &self.name,
            &self.version,
            &self.system,
            &self.url,
            &self.sha256,
        ] {
            hash.update(field.as_bytes());
            hash.update([0]);
        }
        format!("{:x}", hash.finalize())
    }
}

/// Immutable identity of a fetched source. Registry archives have a
/// publisher-provided digest; other sources become locks only after the fetch
/// layer has established a content digest or repository commit. This keeps
/// mutable URLs and local paths from masquerading as reproducible payloads.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PackageLock {
    Registry(ResolvedPayload),
    Archive {
        url: String,
        sha256: String,
    },
    Repository {
        url: String,
        commit: String,
    },
    LocalPath {
        path: NormalizedPath,
        sha256: String,
    },
}

impl PackageLock {
    /// Stable cache identity for the fully resolved source, including its
    /// content digest/commit. Unlocked requests must not use this as a cache
    /// key because a URL or local directory may change underneath them.
    #[must_use]
    pub fn cache_identity(&self) -> String {
        let mut hash = Sha256::new();
        match self {
            Self::Registry(payload) => {
                hash.update(b"registry\0");
                hash.update(payload.cache_identity().as_bytes());
            }
            Self::Archive { url, sha256 } => {
                hash.update(b"archive\0");
                hash.update(url.as_bytes());
                hash.update([0]);
                hash.update(sha256.as_bytes());
            }
            Self::Repository { url, commit } => {
                hash.update(b"repository\0");
                hash.update(url.as_bytes());
                hash.update([0]);
                hash.update(commit.as_bytes());
            }
            Self::LocalPath { path, sha256 } => {
                hash.update(b"local\0");
                hash.update(path.display_slash().as_bytes());
                hash.update([0]);
                hash.update(sha256.as_bytes());
            }
        }
        format!("{:x}", hash.finalize())
    }
}

/// One package declared by a platform manifest, possibly replaced by an
/// explicit `platform_packages` specification. Optionality is preserved:
/// board/framework-specific selection remains the orchestrator's job.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackageRequirement {
    pub name: String,
    pub kind: PackageKind,
    pub optional: bool,
    pub spec: PackageSpec,
}

/// Parse a PlatformIO package specification without consulting native board
/// support. This does not silently turn a registry version into a URL override.
pub fn parse_package_spec(raw: &str) -> Result<PackageSpec> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err(ResolutionError::InvalidSpec("empty value".into()));
    }

    if looks_like_direct_source(raw) {
        return Ok(PackageSpec {
            alias: None,
            source: parse_direct_source(raw)?,
        });
    }

    if let Some((left, right)) = raw.split_once('@') {
        let left = left.trim();
        let right = right.trim();
        if right.is_empty() {
            return Err(ResolutionError::InvalidSpec(raw.into()));
        }
        if looks_like_direct_source(right) || looks_like_github_ref(right) {
            if !valid_segment(left) {
                return Err(ResolutionError::InvalidSpec(raw.into()));
            }
            return Ok(PackageSpec {
                alias: Some(left.into()),
                source: parse_direct_source(right)?,
            });
        }
        return Ok(PackageSpec {
            alias: None,
            source: PackageSource::Registry(parse_registry(left, Some(right))?),
        });
    }

    if looks_like_github_ref(raw) {
        return Ok(PackageSpec {
            alias: None,
            source: parse_direct_source(raw)?,
        });
    }
    Ok(PackageSpec {
        alias: None,
        source: PackageSource::Registry(parse_registry(raw, None)?),
    })
}

fn parse_registry(name: &str, requirement: Option<&str>) -> Result<RegistrySpec> {
    let parts = name.split('/').collect::<Vec<_>>();
    let (owner, name, registry_type) = match parts.as_slice() {
        [name] => (None, *name, None),
        [owner, name] => (Some(*owner), *name, None),
        [first, middle, name] => {
            let first_type = canonical_registry_type(first);
            let middle_type = canonical_registry_type(middle);
            match (first_type, middle_type) {
                (Some(kind), None) => (Some(*middle), *name, Some(kind)),
                // Owner-first is canonical when both segments look like types:
                // registry owner names are not reserved words.
                (_, Some(kind)) => (Some(*first), *name, Some(kind)),
                _ => return Err(ResolutionError::InvalidSpec(name.to_string())),
            }
        }
        _ => return Err(ResolutionError::InvalidSpec(name.to_string())),
    };
    if !valid_segment(name) || owner.is_some_and(|o| !valid_segment(o)) {
        return Err(ResolutionError::InvalidSpec(name.into()));
    }
    if let Some(req) = requirement {
        validate_requirement(req)?;
    }
    Ok(RegistrySpec {
        owner: owner.map(str::to_string),
        name: name.into(),
        requirement: requirement.map(str::to_string),
        registry_type: registry_type.map(str::to_string),
    })
}

fn canonical_registry_type(raw: &str) -> Option<&'static str> {
    match raw {
        "platform" => Some("platform"),
        "tool" | "toolchain" | "framework" | "uploader" | "debugger" => Some("tool"),
        "library" => Some("library"),
        _ => None,
    }
}

fn valid_segment(value: &str) -> bool {
    !matches!(value, "" | "." | "..")
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

fn validate_requirement(req: &str) -> Result<()> {
    for clause in req.split(',') {
        let clause = clause.trim();
        if clause.is_empty() {
            return Err(ResolutionError::InvalidSpec(req.into()));
        }
        if clause == "*" {
            continue;
        }
        if let Some(excluded) = clause.strip_prefix("!=") {
            if Version::parse(excluded).is_err() {
                return Err(ResolutionError::InvalidSpec(req.into()));
            }
        } else if let Some(exact) = clause.strip_prefix('=') {
            if Version::parse(exact).is_err() {
                return Err(ResolutionError::InvalidSpec(req.into()));
            }
        } else if clause.starts_with(['^', '~', '>', '<']) {
            if VersionReq::parse(clause).is_err() {
                return Err(ResolutionError::InvalidSpec(req.into()));
            }
        } else if Version::parse(clause).is_err() {
            return Err(ResolutionError::InvalidSpec(req.into()));
        }
    }
    Ok(())
}

fn looks_like_direct_source(value: &str) -> bool {
    starts_with_any(
        value,
        &[
            "http://", "https://", "git+", "git://", "ssh://", "file://", "./", "../", "/",
        ],
    ) || value.starts_with("git@")
        || (value.len() >= 3
            && value.as_bytes()[0].is_ascii_alphabetic()
            && value.as_bytes()[1] == b':'
            && matches!(value.as_bytes()[2], b'/' | b'\\'))
}

fn starts_with_any(value: &str, prefixes: &[&str]) -> bool {
    prefixes.iter().any(|prefix| value.starts_with(prefix))
}

fn looks_like_github_ref(value: &str) -> bool {
    let Some((repo, reference)) = value.split_once('#') else {
        return false;
    };
    !reference.is_empty() && repo.split('/').count() == 2 && repo.split('/').all(valid_segment)
}

fn parse_direct_source(value: &str) -> Result<PackageSource> {
    if value.starts_with("file://") {
        if value == "file://" {
            return Err(ResolutionError::InvalidSpec(value.into()));
        }
        let path = reqwest::Url::parse(value)
            .ok()
            .and_then(|url| url.to_file_path().ok())
            .ok_or_else(|| ResolutionError::InvalidSpec(value.into()))?;
        return Ok(PackageSource::LocalPath {
            path: NormalizedPath::new(path),
        });
    }
    if starts_with_any(value, &["./", "../", "/"])
        || (value.len() >= 3
            && value.as_bytes()[1] == b':'
            && matches!(value.as_bytes()[2], b'/' | b'\\'))
    {
        return Ok(PackageSource::LocalPath {
            path: NormalizedPath::new(value),
        });
    }
    if looks_like_github_ref(value) {
        let Some((repo, reference)) = value.split_once('#') else {
            return Err(ResolutionError::InvalidSpec(value.into()));
        };
        return Ok(PackageSource::Repository {
            url: format!("https://github.com/{repo}.git"),
            reference: Some(reference.into()),
        });
    }
    let value = value.strip_prefix("git+").unwrap_or(value);
    let (url, reference) = match value.rsplit_once('#') {
        Some((url, reference)) if !reference.is_empty() => (url, Some(reference.into())),
        Some(_) => return Err(ResolutionError::InvalidSpec(value.into())),
        None => (value, None),
    };
    let scheme_ok = starts_with_any(url, &["http://", "https://", "git://", "ssh://"])
        || url.starts_with("git@");
    if !scheme_ok {
        return Err(ResolutionError::InvalidSpec(value.into()));
    }
    let url_without_query = url.split('?').next().unwrap_or(url);
    let archive = [
        ".tar.gz", ".tgz", ".tar.bz2", ".tar.xz", ".txz", ".tar.zst", ".zip",
    ]
    .iter()
    .any(|ext| url_without_query.ends_with(ext));
    if archive {
        return Ok(PackageSource::Archive {
            url: url.into(),
            revision: reference,
        });
    }
    let known_repository = [
        "https://github.com/",
        "https://gitlab.com/",
        "https://bitbucket.org/",
    ]
    .iter()
    .filter_map(|prefix| url.strip_prefix(prefix))
    .any(|path| path.split('/').filter(|part| !part.is_empty()).count() >= 2);
    if url.ends_with(".git")
        || reference.is_some()
        || known_repository
        || starts_with_any(url, &["git://", "ssh://", "git@"])
    {
        return Ok(PackageSource::Repository {
            url: url.into(),
            reference,
        });
    }
    Err(ResolutionError::InvalidSpec(value.into()))
}

/// Construct the metadata endpoint for an owner-qualified registry name.
pub fn registry_api_url(kind: PackageKind, spec: &RegistrySpec) -> Result<String> {
    validate_registry_type(kind, spec)?;
    let owner = spec
        .owner
        .as_deref()
        .ok_or_else(|| ResolutionError::OwnerRequired(spec.name.clone()))?;
    if !valid_segment(owner) || !valid_segment(&spec.name) {
        return Err(ResolutionError::InvalidSpec(format!(
            "{owner}/{}",
            spec.name
        )));
    }
    Ok(format!(
        "{REGISTRY_API}/{owner}/{}/{}",
        kind.registry_type(),
        spec.name
    ))
}

fn validate_registry_type(kind: PackageKind, spec: &RegistrySpec) -> Result<()> {
    if spec
        .registry_type
        .as_deref()
        .is_some_and(|declared| declared != kind.registry_type())
    {
        return Err(ResolutionError::InvalidSpec(format!(
            "{} is a {} package, not a {} package",
            spec.name,
            spec.registry_type.as_deref().unwrap_or_default(),
            kind.registry_type()
        )));
    }
    Ok(())
}

#[derive(Deserialize)]
struct RegistryResponse {
    name: String,
    owner: RegistryOwner,
    versions: Vec<RegistryVersion>,
}

#[derive(Deserialize)]
struct RegistryOwner {
    username: String,
}

#[derive(Deserialize)]
struct RegistryVersion {
    name: String,
    files: Vec<RegistryFile>,
}

#[derive(Deserialize)]
struct RegistryFile {
    /// Host selector; a missing or `null` field means universal, like `"*"`.
    #[serde(default)]
    system: serde_json::Value,
    download_url: String,
    checksum: Option<RegistryChecksum>,
}

#[derive(Deserialize)]
struct RegistryChecksum {
    sha256: Option<String>,
}

fn version_matches(requirement: Option<&str>, version: &str) -> bool {
    let Some(requirement) = requirement else {
        return true;
    };
    let parsed = Version::parse(version).ok();
    requirement.split(',').all(|clause| {
        let clause = clause.trim();
        if clause == "*" {
            return true;
        }
        if let Some(excluded) = clause.strip_prefix("!=") {
            return version != excluded;
        }
        if let Some(exact) = clause.strip_prefix('=') {
            return version == exact;
        }
        if clause.starts_with(['^', '~', '>', '<']) {
            return parsed
                .as_ref()
                .is_some_and(|v| VersionReq::parse(clause).is_ok_and(|req| req.matches(v)));
        }
        version == clause
    })
}

fn file_matches(file: &RegistryFile, system: &str) -> Option<u8> {
    match &file.system {
        serde_json::Value::String(s) if s == system => Some(2),
        serde_json::Value::String(s) if s == "*" => Some(1),
        serde_json::Value::Null => Some(1),
        serde_json::Value::Array(systems) if systems.iter().any(|s| s.as_str() == Some(system)) => {
            Some(2)
        }
        serde_json::Value::Array(systems) if systems.iter().any(|s| s.as_str() == Some("*")) => {
            Some(1)
        }
        _ => None,
    }
}

/// Select one published registry archive for a requested host. Exact pins
/// compare the full version string, including build metadata; ranges choose
/// the highest compatible published version with a payload for that host.
pub fn resolve_registry_json(
    spec: &RegistrySpec,
    kind: PackageKind,
    system: &str,
    metadata_json: &str,
) -> Result<ResolvedPayload> {
    validate_registry_type(kind, spec)?;
    if let Some(requirement) = &spec.requirement {
        validate_requirement(requirement)?;
    }
    let metadata: RegistryResponse = serde_json::from_str(metadata_json)
        .map_err(|e| ResolutionError::InvalidMetadata(e.to_string()))?;
    if !metadata.name.eq_ignore_ascii_case(&spec.name)
        || spec
            .owner
            .as_deref()
            .is_some_and(|o| !metadata.owner.username.eq_ignore_ascii_case(o))
    {
        return Err(ResolutionError::InvalidMetadata(format!(
            "requested {:?}/{} but received {}/{}",
            spec.owner, spec.name, metadata.owner.username, metadata.name
        )));
    }
    let requested = spec.requirement.as_deref().unwrap_or("*");
    let matching: Vec<&RegistryVersion> = metadata
        .versions
        .iter()
        .filter(|v| version_matches(spec.requirement.as_deref(), &v.name))
        .collect();
    if matching.is_empty() {
        return Err(ResolutionError::VersionNotFound {
            package: spec.name.clone(),
            requirement: requested.into(),
        });
    }
    let mut candidates: Vec<(&RegistryVersion, &RegistryFile, u8)> = matching
        .iter()
        .flat_map(|version| {
            version.files.iter().filter_map(move |file| {
                file_matches(file, system).map(|priority| (*version, file, priority))
            })
        })
        .collect();
    if candidates.is_empty() {
        return Err(ResolutionError::UnsupportedHost {
            package: spec.name.clone(),
            version: requested.into(),
            system: system.into(),
        });
    }
    candidates.sort_by(|(a, _, a_priority), (b, _, b_priority)| {
        Version::parse(&a.name)
            .ok()
            .cmp(&Version::parse(&b.name).ok())
            .then_with(|| a.name.cmp(&b.name))
            .then_with(|| a_priority.cmp(b_priority))
    });
    let Some((version, file, _)) = candidates.last() else {
        return Err(ResolutionError::UnsupportedHost {
            package: spec.name.clone(),
            version: requested.into(),
            system: system.into(),
        });
    };
    let sha256 = file
        .checksum
        .as_ref()
        .and_then(|c| c.sha256.as_deref())
        .filter(|hash| hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit()))
        .ok_or_else(|| ResolutionError::InvalidMetadata("missing or invalid SHA-256".into()))?;
    if !file.download_url.starts_with("https://") {
        return Err(ResolutionError::InvalidMetadata(
            "registry download URL is not HTTPS".into(),
        ));
    }
    Ok(ResolvedPayload {
        owner: metadata.owner.username,
        kind,
        name: metadata.name,
        version: version.name.clone(),
        system: system.into(),
        url: file.download_url.clone(),
        sha256: sha256.to_ascii_lowercase(),
    })
}

/// Parse a platform's package declarations and apply explicit package
/// overrides. This returns *all* declarations, including optional packages;
/// board/framework-specific enablement remains with the build orchestrator.
pub fn resolve_platform_requirements(
    manifest_json: &str,
    overrides: &[PackageSpec],
) -> Result<Vec<PackageRequirement>> {
    let manifest: serde_json::Value = serde_json::from_str(manifest_json)
        .map_err(|e| ResolutionError::InvalidMetadata(e.to_string()))?;
    let packages = manifest
        .get("packages")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| {
            ResolutionError::InvalidMetadata("platform.json has no packages object".into())
        })?;
    let mut requirements = Vec::with_capacity(packages.len() + overrides.len());
    for (name, package) in packages {
        if !valid_segment(name) {
            return Err(ResolutionError::InvalidMetadata(format!(
                "invalid package name {name}"
            )));
        }
        let version = package
            .get("version")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| ResolutionError::InvalidMetadata(format!("{name} has no version")))?;
        let owner = package.get("owner").and_then(serde_json::Value::as_str);
        let package_type = package.get("type").and_then(serde_json::Value::as_str);
        let kind = match package_type {
            Some("platform") => PackageKind::Platform,
            Some("framework") => PackageKind::Framework,
            Some("tool" | "toolchain" | "uploader" | "debugger") => PackageKind::Tool,
            Some("library") => PackageKind::Library,
            None if name.starts_with("framework-") => PackageKind::Framework,
            None => PackageKind::Tool,
            Some(other) => {
                return Err(ResolutionError::InvalidMetadata(format!(
                    "unsupported package type {other}"
                )));
            }
        };
        let optional = package
            .get("optional")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        let mut spec = parse_package_spec(&format!("{name}@{version}"))?;
        if let PackageSource::Registry(registry) = &mut spec.source {
            registry.owner = owner.map(str::to_string);
        }
        if let Some(override_spec) = overrides
            .iter()
            .find(|o| o.package_name() == Some(name.as_str()))
        {
            spec = override_spec.clone();
            if let PackageSource::Registry(registry) = &mut spec.source {
                if registry.owner.is_none() {
                    registry.owner = owner.map(str::to_string);
                }
            }
        }
        requirements.push(PackageRequirement {
            name: name.clone(),
            kind,
            optional,
            spec,
        });
    }
    for override_spec in overrides {
        let Some(name) = override_spec.package_name() else {
            continue;
        };
        if requirements.iter().any(|r| r.name == name) {
            continue;
        }
        let kind = if name.starts_with("framework-") {
            PackageKind::Framework
        } else if name.starts_with("platform-") {
            PackageKind::Platform
        } else {
            PackageKind::Tool
        };
        requirements.push(PackageRequirement {
            name: name.into(),
            kind,
            optional: false,
            spec: override_spec.clone(),
        });
    }
    Ok(requirements)
}

/// Require a selected native package from the platform manifest after
/// explicit overrides have been applied. An absent package is an
/// incompatibility, never permission to use an unrelated adapter default.
pub fn require_platform_package<'a>(
    requirements: &'a [PackageRequirement],
    name: &str,
) -> Result<&'a PackageRequirement> {
    requirements
        .iter()
        .find(|requirement| requirement.name == name)
        .ok_or_else(|| ResolutionError::MissingPackage(name.to_string()))
}

/// Return a package's sole alternate version requirement from a selected
/// platform manifest. Some PlatformIO builders choose that alternate for a
/// particular board/core. If multiple alternatives exist, their meaning is
/// board-specific and must not be guessed from ordering.
pub fn sole_optional_manifest_version(manifest_json: &str, name: &str) -> Result<String> {
    let manifest: serde_json::Value = serde_json::from_str(manifest_json)
        .map_err(|error| ResolutionError::InvalidMetadata(error.to_string()))?;
    let package = manifest
        .get("packages")
        .and_then(|packages| packages.get(name))
        .ok_or_else(|| ResolutionError::MissingPackage(name.to_string()))?;
    let alternatives = package
        .get("optionalVersions")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| {
            ResolutionError::InvalidMetadata(format!(
                "{name} has no optionalVersions for this platform"
            ))
        })?;
    let [only] = alternatives.as_slice() else {
        return Err(ResolutionError::InvalidMetadata(format!(
            "{name} has {} optionalVersions; board-specific selection is ambiguous",
            alternatives.len()
        )));
    };
    let requirement = only.as_str().ok_or_else(|| {
        ResolutionError::InvalidMetadata(format!("{name} optional version is not a string"))
    })?;
    validate_requirement(requirement)?;
    Ok(requirement.to_string())
}
