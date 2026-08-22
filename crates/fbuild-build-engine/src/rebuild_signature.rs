//! Rebuild-signature fingerprints for incremental compile invalidation.
//!
//! Extracted from `compiler.rs` to keep both files under the workspace's
//! 1000-LOC limit; all public items are re-exported from
//! [`crate::compiler`] so external paths are unchanged.

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use fbuild_core::path::NormalizedPath;
use sha2::{Digest, Sha256};

static COMPILER_IDENTITY_CACHE: OnceLock<Mutex<HashMap<PathBuf, String>>> = OnceLock::new();

/// Stable fingerprint of a compile invocation, used for incremental rebuild
/// invalidation.
///
/// `unflags` are applied to `flags` and `extra_flags` **inside** this function
/// (matching the write side, where `compile_c`/`compile_cpp` run
/// `apply_compile_unflags` over exactly those two groups before compiling).
/// Centralizing the stripping here — rather than at each caller — is load
/// bearing: every `Compiler::rebuild_signature` override funnels through this
/// one function, so none of them can silently forget to strip `build_unflags`
/// and drift from the written signature (FastLED/fbuild#970). `pre_flags`
/// (e.g. ESP32 include flags) are **not** unflag-filtered, mirroring the write
/// side. Platforms with no `build_unflags` pass an empty slice → the hash is
/// byte-identical to before, so no signature churn for them.
pub fn build_rebuild_signature(
    compiler_path: &Path,
    flags: &[String],
    pre_flags: &[String],
    extra_flags: &[String],
    unflags: &[String],
) -> String {
    build_rebuild_signature_with_normalizer(
        compiler_path,
        flags,
        pre_flags,
        extra_flags,
        unflags,
        &normalize_signature_value,
    )
}

/// Variant of [`build_rebuild_signature`] for global artifact cache keys.
///
/// Any absolute path under `project_dir` is reduced to `.project/<relative>`
/// before hashing, so two fresh checkouts with the same project layout produce
/// the same cache key even when their absolute roots or basenames differ.
pub fn build_rebuild_signature_for_project(
    project_dir: &Path,
    compiler_path: &Path,
    flags: &[String],
    pre_flags: &[String],
    extra_flags: &[String],
    unflags: &[String],
) -> String {
    let normalize = |value: &str| normalize_signature_value_for_project(value, project_dir);
    build_rebuild_signature_with_normalizer(
        compiler_path,
        flags,
        pre_flags,
        extra_flags,
        unflags,
        &normalize,
    )
}

/// Variant of [`build_rebuild_signature`] anchored to a compile workspace.
///
/// Absolute path-bearing flag values that live *inside* `compile_cwd` are
/// relativized against it before hashing — exactly the transform the executed
/// argv undergoes in [`crate::compiler::compile_source`] — so two workspaces
/// that would run byte-identical compiler commands hash identically even when
/// their absolute roots differ. Values outside the workspace fall back to the
/// legacy project-independent normalization. With `compile_cwd = None` this is
/// byte-for-byte [`build_rebuild_signature`].
///
/// FastLED/fbuild#1346: without the workspace anchor, sibling stage-2
/// workspaces (`.tmpX/s0/src` vs `.tmpX/s1/src`) hit the last-two-components
/// fallback and produced different signatures from identical effective
/// commands, so every seeded framework object failed its `.cmdhash` check and
/// stage 2 recompiled the whole framework.
pub fn build_rebuild_signature_for_workspace(
    compile_cwd: Option<&Path>,
    compiler_path: &Path,
    flags: &[String],
    pre_flags: &[String],
    extra_flags: &[String],
    unflags: &[String],
) -> String {
    let normalize = |value: &str| match compile_cwd {
        Some(cwd) => normalize_signature_value_for_workspace(value, cwd),
        None => normalize_signature_value(value),
    };
    build_rebuild_signature_with_normalizer(
        compiler_path,
        flags,
        pre_flags,
        extra_flags,
        unflags,
        &normalize,
    )
}

fn build_rebuild_signature_with_normalizer(
    compiler_path: &Path,
    flags: &[String],
    pre_flags: &[String],
    extra_flags: &[String],
    unflags: &[String],
    normalize_value: &dyn Fn(&str) -> String,
) -> String {
    let strip = |group: &[String]| -> Vec<String> {
        if unflags.is_empty() {
            return group.to_vec();
        }
        let mut filtered = group.to_vec();
        crate::pipeline::remove_unflagged_tokens(&mut filtered, unflags);
        filtered
    };
    let flags = strip(flags);
    let extra_flags = strip(extra_flags);

    let mut hasher = Sha256::new();
    hasher.update(compiler_identity(compiler_path).as_bytes());
    hasher.update([0]);
    for group in [flags.as_slice(), pre_flags, extra_flags.as_slice()] {
        hash_signature_group(&mut hasher, group, normalize_value);
        hasher.update([0xff]);
    }
    format!("{:x}", hasher.finalize())
}

fn hash_signature_group(
    hasher: &mut Sha256,
    group: &[String],
    normalize_value: &dyn Fn(&str) -> String,
) {
    let mut expects_path_value = false;
    for flag in group {
        let normalized = if expects_path_value {
            expects_path_value = false;
            normalize_value(flag)
        } else {
            expects_path_value = is_split_path_flag(flag);
            normalize_signature_flag(flag, normalize_value)
        };
        hasher.update(normalized.as_bytes());
        hasher.update([0]);
    }
}

fn is_split_path_flag(flag: &str) -> bool {
    matches!(
        flag,
        "-I" | "-isystem" | "-iquote" | "-include" | "--sysroot"
    )
}

fn normalize_signature_flag(flag: &str, normalize_value: &dyn Fn(&str) -> String) -> String {
    for prefix in ["-I", "-isystem=", "-iquote=", "-include=", "--sysroot="] {
        if let Some(value) = flag.strip_prefix(prefix) {
            return format!("{prefix}{}", normalize_value(value));
        }
    }
    flag.to_string()
}

fn normalize_signature_value(value: &str) -> String {
    if value.is_empty() {
        return String::new();
    }
    let path = Path::new(value);
    if !looks_like_absolute_path(path, value) {
        return value.to_string();
    }
    normalize_signature_path(path)
}

/// Workspace-anchored counterpart of [`normalize_signature_value`].
///
/// Absolute values inside the compile workspace relativize to their
/// workspace-relative form (the same string the compiler actually sees on its
/// command line); everything else keeps the legacy project-independent
/// normalization.
fn normalize_signature_value_for_workspace(value: &str, compile_cwd: &Path) -> String {
    if value.is_empty() {
        return String::new();
    }
    let path = Path::new(value);
    if !looks_like_absolute_path(path, value) {
        return value.to_string();
    }
    let arg = fbuild_core::path::path_arg_for_compile_cwd(path, compile_cwd);
    if !looks_like_absolute_path(Path::new(&arg), &arg) {
        return arg;
    }
    normalize_signature_path(path)
}

fn normalize_signature_value_for_project(value: &str, project_dir: &Path) -> String {
    if value.is_empty() {
        return String::new();
    }
    let path = Path::new(value);
    if !looks_like_absolute_path(path, value) {
        return value.to_string();
    }
    let arg = fbuild_core::path::path_arg_for_compile_cwd(path, project_dir);
    if !looks_like_absolute_path(Path::new(&arg), &arg) {
        if arg == "." {
            ".project".to_string()
        } else {
            format!(".project/{arg}")
        }
    } else {
        normalize_signature_path(path)
    }
}

fn normalize_signature_path(path: &Path) -> String {
    let normalized = normalize_signature_components(path);
    if let Some(index) = normalized
        .iter()
        .position(|component| component.eq_ignore_ascii_case(".fbuild"))
    {
        return normalized[index..].join("/");
    }
    if let Some(index) = normalized
        .iter()
        .position(|component| component.eq_ignore_ascii_case(".build"))
    {
        return normalized[index..].join("/");
    }
    const TAIL_COMPONENTS: usize = 2;
    let start = normalized.len().saturating_sub(TAIL_COMPONENTS);
    normalized[start..].join("/")
}

fn normalize_signature_components(path: &Path) -> Vec<String> {
    // FastLED/fbuild#911 — every per-component slash rewrite delegates
    // to `NormalizedPath::display_slash()`, which owns the Windows
    // `\` → `/` transform (and the UNC prefix strip) for the workspace.
    // Same hand-rolled anti-pattern the compile pipeline used to have.
    path.components()
        .filter_map(|component| match component {
            Component::Prefix(prefix) => {
                Some(NormalizedPath::new(prefix.as_os_str()).display_slash())
            }
            Component::RootDir => None,
            Component::CurDir => None,
            Component::ParentDir => Some("..".to_string()),
            Component::Normal(value) => Some(NormalizedPath::new(value).display_slash()),
        })
        .collect()
}

fn looks_like_absolute_path(path: &Path, raw: &str) -> bool {
    path.is_absolute()
        || path.has_root()
        || raw.starts_with('/')
        || raw.starts_with('\\')
        || raw.as_bytes().get(1) == Some(&b':')
}

fn compiler_identity(path: &Path) -> String {
    let cache = COMPILER_IDENTITY_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(identity) = cache
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(path)
        .cloned()
    {
        return identity;
    }

    let stem = path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_string();
    let version = compiler_version(path);
    let identity = format!("{stem}\0{version}");
    cache
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(path.to_path_buf(), identity.clone());
    identity
}

fn compiler_version(path: &Path) -> String {
    // FastLED/fbuild#820 (Phase B of #813): `fbuild_core::subprocess::
    // run_command` is now `async`. `compiler_version` is called from
    // the sync `rebuild_signature` trait method (which is in turn
    // called from sync rebuild-check code paths), so we bridge to the
    // ambient tokio runtime via `block_in_place` + `block_on`. This is
    // safe because the daemon runs on a multi-thread tokio runtime and
    // `block_in_place` permits this exact pattern.
    let program = path.to_string_lossy().to_string();
    let result = match tokio::runtime::Handle::try_current() {
        Ok(handle) => tokio::task::block_in_place(|| {
            handle.block_on(async {
                let args = [program.as_str(), "-dumpversion"];
                // FastLED/fbuild#809: `gcc -dumpversion` is trivial; a
                // hung toolchain binary (corrupt EXE, missing-DLL hang
                // on Windows) should not block the whole pipeline.
                fbuild_core::subprocess::run_command(
                    &args,
                    None,
                    None,
                    Some(std::time::Duration::from_secs(5)),
                )
                .await
            })
        }),
        Err(_) => {
            // No ambient runtime — happens in unit-test contexts that
            // don't spin up a tokio runtime. Returning an empty version
            // is a graceful degradation: rebuild-signature loses the
            // compiler-version contribution but still encodes path +
            // flags, which is enough for the tests that don't touch a
            // real toolchain.
            return String::new();
        }
    };
    match result {
        Ok(output) if output.success() => output.stdout.trim().to_string(),
        _ => String::new(),
    }
}
