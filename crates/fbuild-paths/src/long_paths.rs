//! Windows long-path policy and immutable cache/build fallback selection.
//!
//! Entry points call [`initialize`] before cache or daemon work. The resolver
//! never changes the environment or emits output; each caller delivers the
//! advisory through its own terminal or HTTP output channel.

use std::ffi::OsString;
use std::io::Write;
use std::path::Path;
use std::sync::{Mutex, OnceLock};

use fbuild_core::path::NormalizedPath;
use fbuild_core::platform::fs;
use fbuild_core::{FbuildError, Result};
use serde::{Deserialize, Serialize};

/// Leaves at least 180 characters for SDK/package paths below a cache root.
const MAX_SHORT_ROOT_UNITS: usize = 64;
/// Conservative legacy directory limit, including the terminating NUL.
const MAX_LEGACY_DIRECTORY_UNITS: usize = 248;
const MARKER_FILE: &str = "windows-short-paths.json";
const MARKER_SCHEMA: u32 = 1;
const HASH_HEX_UNITS: usize = 12;

static RESOLUTION: OnceLock<Resolution> = OnceLock::new();
static INITIALIZE_LOCK: Mutex<()> = Mutex::new(());

/// Observed system policy, including an unreadable or malformed value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", content = "reason", rename_all = "kebab-case")]
pub enum Observation {
    NotApplicable,
    Enabled,
    Disabled,
    Unknown(String),
}

impl Observation {
    /// Stable state key suitable for comparing daemon startup and current policy.
    pub fn status_key(&self) -> &'static str {
        match self {
            Self::NotApplicable => "not-applicable",
            Self::Enabled => "enabled",
            Self::Disabled => "disabled",
            Self::Unknown(_) => "unknown",
        }
    }

    /// An unreadable policy receives the same conservative fallback as disabled.
    pub fn requires_fallback(&self) -> bool {
        matches!(self, Self::Disabled | Self::Unknown(_))
    }
}

/// Paths selected once per process. Existing operations retain this snapshot
/// even when another process changes the system setting or shell environment.
#[derive(Debug, Clone, Serialize)]
pub struct Resolution {
    pub observation: Observation,
    pub home: NormalizedPath,
    pub fbuild_root: NormalizedPath,
    pub dev_mode: bool,
    pub requested_cache_root: NormalizedPath,
    pub cache_root: NormalizedPath,
    pub scratch_root: NormalizedPath,
    pub short_build_root: Option<NormalizedPath>,
    pub build_override: Option<NormalizedPath>,
    pub cache_dir_source: &'static str,
}

impl Resolution {
    /// Plain advisory text; callers choose how to render yellow warnings.
    pub fn advisory(&self) -> Option<String> {
        let reason = match &self.observation {
            Observation::Disabled => "Windows long-path support is disabled.".to_string(),
            Observation::Unknown(reason) => {
                format!("Windows long-path support could not be checked: {reason}.")
            }
            Observation::NotApplicable | Observation::Enabled => return None,
        };
        let requested = if NormalizedPath::new(&self.requested_cache_root)
            != NormalizedPath::new(&self.cache_root)
        {
            format!(" Requested cache: {}.", self.requested_cache_root.display())
        } else {
            String::new()
        };
        let builds = self
            .short_build_root
            .as_ref()
            .map_or_else(String::new, |root| {
                format!(
                    " Deep build outputs are redirected under {}.",
                    root.display()
                )
            });
        Some(format!(
            "{reason} Using short-path fallback cache: {}. Extraction scratch: {}.{requested}{builds} Run fbuild --fix-win to enable Windows long paths.",
            self.cache_root.display(),
            self.scratch_root.display(),
        ))
    }

    /// Redirect a deep build root to a directory keyed by the normalized
    /// project and requested output root.
    /// The `.fbuild/build` suffix preserves the compiler workspace convention.
    pub fn resolve_build_root(&self, project_dir: &Path, requested_root: &Path) -> NormalizedPath {
        let Some(base) = &self.short_build_root else {
            return NormalizedPath::new(requested_root);
        };
        let requested = absolute_normalized(requested_root)
            .unwrap_or_else(|_| NormalizedPath::new(requested_root));
        if is_within(&requested, base) {
            return requested;
        }
        if fs::legacy_path_units(&requested) < MAX_SHORT_ROOT_UNITS {
            return requested;
        }
        let project = project_dir
            .canonicalize()
            .ok()
            .and_then(|path| absolute_normalized(&path).ok())
            .or_else(|| absolute_normalized(project_dir).ok())
            .unwrap_or_else(|| NormalizedPath::new(project_dir));
        base.join(build_root_hash(&project, &requested))
            .join(crate::FBUILD_DIR_NAME)
            .join(crate::BUILD_DIR_NAME)
    }

    /// Reject paths that remain too deep after fallback selection. Applies to
    /// directories as well as files so extraction cannot cross the stricter limit.
    pub fn validate_legacy_path(&self, path: &Path) -> Result<()> {
        if !self.observation.requires_fallback() {
            return Ok(());
        }
        let absolute = absolute_normalized(path)?;
        let units = fs::legacy_path_units(&absolute) + 1;
        if units > MAX_LEGACY_DIRECTORY_UNITS {
            return Err(FbuildError::ConfigError(format!(
                "Windows short-path fallback cannot safely use {} ({units} path units including NUL; limit {MAX_LEGACY_DIRECTORY_UNITS}). Run fbuild --fix-win, or shorten the project/package path.",
                absolute.display()
            )));
        }
        Ok(())
    }

    /// Secure automatic output directories before any build writes. User
    /// configured cache roots and project-local outputs retain their own ACLs.
    pub fn prepare_build_directory(&self, path: &Path) -> Result<()> {
        self.validate_legacy_path(path)?;
        if self.cache_dir_source != "windows-short-path-fallback" {
            return Ok(());
        }
        let Some(base) = &self.short_build_root else {
            return Ok(());
        };
        let absolute = absolute_normalized(path)?;
        if is_within(&absolute, base) {
            // Verify each selected ancestor before creating its children.
            // This also prevents portable fixture helpers from recursively
            // creating through a linked ancestor before the final check.
            let base_depth = base.as_path().components().count();
            let directories: Vec<&Path> = absolute
                .ancestors()
                .take_while(|path| path.components().count() >= base_depth)
                .collect();
            for directory in directories.into_iter().rev() {
                fs::ensure_private_directory(directory).map_err(|error| {
                    fallback_error(
                        &format!("secure build directory {}", directory.display()),
                        &error,
                    )
                })?;
            }
        }
        Ok(())
    }
}

/// Read the current registry policy without changing this process's snapshot.
pub fn probe() -> Observation {
    match fs::long_path_support() {
        Ok(fs::LongPathSupport::NotApplicable) => Observation::NotApplicable,
        Ok(fs::LongPathSupport::Enabled) => Observation::Enabled,
        Ok(fs::LongPathSupport::Disabled) => Observation::Disabled,
        Err(error) => Observation::Unknown(error.to_string()),
    }
}

/// Capture policy and path overrides exactly once, before any cache/daemon work.
/// A failed initialization remains retryable and never installs a partial snapshot.
pub fn initialize() -> Result<&'static Resolution> {
    if let Some(resolution) = RESOLUTION.get() {
        return Ok(resolution);
    }
    let _guard = INITIALIZE_LOCK.lock().map_err(|error| {
        FbuildError::Other(format!("long-path initialization lock failed: {error}"))
    })?;
    if let Some(resolution) = RESOLUTION.get() {
        return Ok(resolution);
    }
    let observation = probe();
    let home = crate::normalized_home()
        .ok_or_else(|| FbuildError::ConfigError("could not determine home directory".into()))?;
    let home = absolute_normalized(&home)?;
    let dev_mode = std::env::var("FBUILD_DEV_MODE").is_ok_and(|value| value == "1");
    let fbuild_root = home
        .join(crate::FBUILD_DIR_NAME)
        .join(if dev_mode { "dev" } else { "prod" });
    let inputs = Inputs {
        #[cfg(test)]
        short_base: fbuild_root.clone(),
        home,
        fbuild_root,
        cache_override: override_from_value(std::env::var_os("FBUILD_CACHE_DIR"))?,
        build_override: override_from_value(std::env::var_os("FBUILD_BUILD_DIR"))?,
        dev_mode,
        observation,
    };
    let resolution = resolve_with_short_base(inputs, fs::short_cache_base)?;
    Ok(RESOLUTION.get_or_init(|| resolution))
}

/// Inspect the startup snapshot without triggering policy reads or filesystem work.
pub fn current_resolution() -> Option<&'static Resolution> {
    RESOLUTION.get()
}

/// Advisory from the startup snapshot, if fallback is active.
pub fn advisory() -> Option<String> {
    current_resolution().and_then(Resolution::advisory)
}

/// Validate paths using the startup snapshot; uninitialized legacy callers retain
/// their existing behavior until their entry point explicitly initializes policy.
pub fn validate_legacy_path(path: &Path) -> Result<()> {
    current_resolution().map_or(Ok(()), |resolution| resolution.validate_legacy_path(path))
}

/// Prepare automatic fallback output hierarchies using the startup snapshot.
pub fn prepare_build_directory(path: &Path) -> Result<()> {
    current_resolution().map_or(Ok(()), |resolution| {
        resolution.prepare_build_directory(path)
    })
}

/// Resolve a potentially deep build root using the initialized snapshot.
pub fn resolve_build_root(project_dir: &Path, requested_root: &Path) -> NormalizedPath {
    current_resolution().map_or_else(
        || NormalizedPath::new(requested_root),
        |resolution| resolution.resolve_build_root(project_dir, requested_root),
    )
}

struct Inputs {
    home: NormalizedPath,
    fbuild_root: NormalizedPath,
    #[cfg(test)]
    short_base: NormalizedPath,
    cache_override: Option<NormalizedPath>,
    build_override: Option<NormalizedPath>,
    dev_mode: bool,
    observation: Observation,
}

#[derive(Debug, Serialize, Deserialize)]
struct SelectionMarker {
    schema: u32,
    cache_root: NormalizedPath,
    scratch_root: NormalizedPath,
}

#[cfg(test)]
fn resolve(input: Inputs) -> Result<Resolution> {
    let short_base = input.short_base.clone();
    resolve_with_short_base(input, || Ok(short_base))
}

fn resolve_with_short_base<F>(input: Inputs, short_base: F) -> Result<Resolution>
where
    F: FnOnce() -> std::io::Result<NormalizedPath>,
{
    let requested = input
        .cache_override
        .as_ref()
        .unwrap_or(&input.fbuild_root.join("cache"))
        .clone();
    let requested_cache_root = absolute_normalized(&requested)?;
    let build_override = input
        .build_override
        .as_ref()
        .map(|path| absolute_normalized(path))
        .transpose()?;
    let mut resolution = Resolution {
        observation: input.observation.clone(),
        home: input.home.clone(),
        fbuild_root: input.fbuild_root.clone(),
        dev_mode: input.dev_mode,
        requested_cache_root: requested_cache_root.clone(),
        cache_root: requested_cache_root,
        scratch_root: input.fbuild_root.join("tmp"),
        short_build_root: None,
        build_override,
        cache_dir_source: if input.cache_override.is_some() {
            "FBUILD_CACHE_DIR"
        } else {
            "default"
        },
    };
    if matches!(input.observation, Observation::NotApplicable) {
        return Ok(resolution);
    }
    let marker_path = input.fbuild_root.join(MARKER_FILE);
    if !input.observation.requires_fallback() {
        if input.cache_override.is_none() {
            if let Some(marker) = read_marker(&marker_path)? {
                let roots = automatic_roots(&input, short_base)?;
                validate_marker(&marker_path, &marker, &roots.cache, &roots.scratch)?;
                ensure_automatic_roots(&roots, false)?;
                resolution.cache_root = marker.cache_root;
                resolution.scratch_root = marker.scratch_root;
                resolution.cache_dir_source = "retained-windows-short-path-fallback";
            }
        }
        return Ok(resolution);
    }
    if input.cache_override.is_some()
        && fs::legacy_path_units(&resolution.requested_cache_root) < MAX_SHORT_ROOT_UNITS
    {
        // This root is intentionally configured by the user. Do not rewrite its
        // ACL or depend on an automatic drive-root location on restricted hosts.
        let scratch = resolution.cache_root.join("t");
        let builds = resolution.cache_root.join("b");
        validate_short_root(&scratch)?;
        validate_short_root(&builds)?;
        ensure_writable(&resolution.cache_root)?;
        ensure_sidecar(&scratch)?;
        ensure_sidecar(&builds)?;
        resolution.scratch_root = scratch;
        resolution.short_build_root = Some(builds);
    } else {
        let roots = automatic_roots(&input, short_base)?;
        validate_short_root(&roots.cache)?;
        validate_short_root(&roots.scratch)?;
        ensure_automatic_roots(&roots, true)?;
        resolution.cache_root = roots.cache.clone();
        resolution.scratch_root = roots.scratch.clone();
        resolution.short_build_root = Some(roots.builds);
        resolution.cache_dir_source = "windows-short-path-fallback";
        write_marker(
            &marker_path,
            &SelectionMarker {
                schema: MARKER_SCHEMA,
                cache_root: roots.cache,
                scratch_root: roots.scratch,
            },
        )?;
    }
    Ok(resolution)
}

struct ShortRoots {
    user: NormalizedPath,
    mode: NormalizedPath,
    cache: NormalizedPath,
    scratch: NormalizedPath,
    builds: NormalizedPath,
}

fn automatic_roots<F>(input: &Inputs, short_base: F) -> Result<ShortRoots>
where
    F: FnOnce() -> std::io::Result<NormalizedPath>,
{
    let base = short_base().map_err(|error| fallback_error("resolve the short root", &error))?;
    let user = absolute_normalized(&base)?.join(format!("fb-{}", path_hash(&input.home)));
    let mode = user.join(if input.dev_mode { "dev" } else { "prod" });
    Ok(ShortRoots {
        cache: mode.join("cache"),
        scratch: mode.join("tmp"),
        builds: mode.join("b"),
        mode,
        user,
    })
}

fn ensure_automatic_roots(roots: &ShortRoots, include_builds: bool) -> Result<()> {
    for path in [&roots.user, &roots.mode, &roots.cache, &roots.scratch] {
        fs::ensure_private_directory(path).map_err(|error| {
            fallback_error(
                &format!("secure private directory {}", path.display()),
                &error,
            )
        })?;
    }
    ensure_writable(&roots.cache)?;
    ensure_writable(&roots.scratch)?;
    if include_builds {
        fs::ensure_private_directory(&roots.builds).map_err(|error| {
            fallback_error(
                &format!("secure private directory {}", roots.builds.display()),
                &error,
            )
        })?;
        ensure_writable(&roots.builds)?;
    }
    Ok(())
}

fn ensure_sidecar(path: &Path) -> Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => {
            if fs::is_link_or_reparse(path)? || !metadata.is_dir() {
                return Err(FbuildError::ConfigError(format!(
                    "Windows short-path fallback sidecar {} must be a real directory, not a link or file. Choose another short FBUILD_CACHE_DIR or run fbuild --fix-win.",
                    path.display()
                )));
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            if let Err(error) = std::fs::create_dir(path) {
                if error.kind() != std::io::ErrorKind::AlreadyExists {
                    return Err(fallback_error(
                        &format!("create sidecar {}", path.display()),
                        &error,
                    ));
                }
            }
            // A concurrent initializer may have created it after our lookup.
            // Recheck the winner before allowing the writability probe.
            let metadata = std::fs::symlink_metadata(path)?;
            if fs::is_link_or_reparse(path)? || !metadata.is_dir() {
                return Err(FbuildError::ConfigError(format!(
                    "Windows short-path fallback sidecar {} must be a real directory, not a link or file. Choose another short FBUILD_CACHE_DIR or run fbuild --fix-win.",
                    path.display()
                )));
            }
        }
        Err(error) => {
            return Err(fallback_error(
                &format!("inspect sidecar {}", path.display()),
                &error,
            ));
        }
    }
    ensure_writable(path)
}

fn absolute_normalized(path: &Path) -> Result<NormalizedPath> {
    if path.as_os_str().is_empty() {
        return Err(FbuildError::ConfigError(
            "fbuild path overrides must not be empty".into(),
        ));
    }
    let absolute = std::path::absolute(path)?;
    Ok(NormalizedPath::new(fs::strip_extended_prefix(&absolute)))
}

/// Resolve raw override spellings before normalization can collapse `.` or
/// `child/..` into an empty relative path. Actual empty values remain errors.
fn override_from_value(value: Option<OsString>) -> Result<Option<NormalizedPath>> {
    value
        .as_deref()
        .map(|raw| absolute_normalized(Path::new(raw)))
        .transpose()
}

/// Lexical, component-bounded membership never follows an untrusted link.
fn is_within(path: &NormalizedPath, root: &NormalizedPath) -> bool {
    path.key()
        .strip_prefix(root.key())
        .is_some_and(|tail| tail.is_empty() || tail.starts_with('/'))
}

fn path_hash(path: &Path) -> String {
    let normalized = NormalizedPath::new(path);
    blake3::hash(normalized.key().as_bytes()).to_hex().as_str()[..HASH_HEX_UNITS].to_string()
}

fn build_root_hash(project: &Path, requested_root: &Path) -> String {
    let project = NormalizedPath::new(project);
    let requested_root = NormalizedPath::new(requested_root);
    let mut hasher = blake3::Hasher::new();
    // Length framing keeps the two identity dimensions distinct even when
    // pathname characters would otherwise make a textual separator ambiguous.
    for key in [project.key(), requested_root.key()] {
        hasher.update(&(key.len() as u64).to_le_bytes());
        hasher.update(key.as_bytes());
    }
    hasher.finalize().to_hex().as_str()[..HASH_HEX_UNITS].to_string()
}

fn validate_short_root(path: &Path) -> Result<()> {
    let units = fs::legacy_path_units(path) + 1;
    if units > MAX_SHORT_ROOT_UNITS {
        return Err(FbuildError::ConfigError(format!(
            "Windows short-path fallback root {} is too long ({units} path units including NUL; maximum {MAX_SHORT_ROOT_UNITS} to reserve SDK path space). Run fbuild --fix-win or set FBUILD_CACHE_DIR to a shorter writable location.",
            path.display()
        )));
    }
    Ok(())
}

fn ensure_writable(path: &Path) -> Result<()> {
    std::fs::create_dir_all(path)
        .map_err(|error| fallback_error(&format!("create {}", path.display()), &error))?;
    let mut probe = tempfile::Builder::new()
        .prefix(".write-probe-")
        .tempfile_in(path)
        .map_err(|error| fallback_error(&format!("write to {}", path.display()), &error))?;
    probe
        .write_all(b"fbuild")
        .and_then(|()| probe.as_file().sync_all())
        .map_err(|error| fallback_error(&format!("write to {}", path.display()), &error))?;
    Ok(())
}

fn fallback_error(action: &str, error: &std::io::Error) -> FbuildError {
    FbuildError::ConfigError(format!(
        "Cannot establish Windows short-path fallback: unable to {action}: {error}. Run fbuild --fix-win, or choose a writable short FBUILD_CACHE_DIR."
    ))
}

fn read_marker(path: &Path) -> Result<Option<SelectionMarker>> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(fallback_error(
                &format!("read cache selection {}", path.display()),
                &error,
            ));
        }
    };
    let marker: SelectionMarker = serde_json::from_slice(&bytes).map_err(|error| {
        FbuildError::ConfigError(format!(
            "Invalid Windows short-path cache selection {}: {error}",
            path.display()
        ))
    })?;
    Ok(Some(marker))
}

fn validate_marker(
    path: &Path,
    marker: &SelectionMarker,
    expected_cache: &Path,
    expected_scratch: &Path,
) -> Result<()> {
    if marker.schema != MARKER_SCHEMA
        || NormalizedPath::new(&marker.cache_root) != NormalizedPath::new(expected_cache)
        || NormalizedPath::new(&marker.scratch_root) != NormalizedPath::new(expected_scratch)
    {
        return Err(FbuildError::ConfigError(format!(
            "Invalid Windows short-path cache selection {}: the saved roots do not match this user's mode and short-path root",
            path.display()
        )));
    }
    Ok(())
}

fn write_marker(path: &Path, marker: &SelectionMarker) -> Result<()> {
    if let Some(existing) = read_marker(path)? {
        return validate_marker(path, &existing, &marker.cache_root, &marker.scratch_root);
    }
    let parent = path
        .parent()
        .ok_or_else(|| FbuildError::ConfigError("cache selection marker has no parent".into()))?;
    std::fs::create_dir_all(parent).map_err(|error| {
        fallback_error(
            &format!("create cache selection directory {}", parent.display()),
            &error,
        )
    })?;
    let bytes =
        serde_json::to_vec(marker).map_err(|error| FbuildError::Other(error.to_string()))?;
    let mut pending = tempfile::Builder::new()
        .prefix(".short-paths-")
        .tempfile_in(parent)
        .map_err(|error| {
            fallback_error(&format!("write cache selection {}", path.display()), &error)
        })?;
    pending
        .write_all(&bytes)
        .and_then(|()| pending.as_file().sync_all())
        .map_err(|error| {
            fallback_error(&format!("write cache selection {}", path.display()), &error)
        })?;
    match pending.persist_noclobber(path) {
        Ok(_) => Ok(()),
        Err(error) => {
            // Another process may have installed the same complete selection
            // since our initial read. Validate its winner without replacing it.
            if let Some(existing) = read_marker(path)? {
                validate_marker(path, &existing, &marker.cache_root, &marker.scratch_root)
            } else {
                Err(fallback_error(
                    &format!("install cache selection {}", path.display()),
                    &error.error,
                ))
            }
        }
    }
}

#[cfg(test)]
mod tests;
