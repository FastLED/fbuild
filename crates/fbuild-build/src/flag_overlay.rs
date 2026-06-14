use std::path::{Path, PathBuf};

/// Language-aware extra compiler flags.
///
/// PlatformIO scripts can mutate common compile scopes (`CCFLAGS`,
/// `CPPDEFINES`, `CPPPATH`) plus language-specific scopes (`CFLAGS`,
/// `CXXFLAGS`, `ASFLAGS`). Native fbuild needs to preserve that separation
/// when replaying the effective build state.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LanguageExtraFlags {
    pub common: Vec<String>,
    pub c: Vec<String>,
    pub cxx: Vec<String>,
    pub asm: Vec<String>,
}

impl LanguageExtraFlags {
    pub fn is_empty(&self) -> bool {
        self.common.is_empty() && self.c.is_empty() && self.cxx.is_empty() && self.asm.is_empty()
    }

    pub fn extend(&mut self, other: &Self) {
        self.common.extend(other.common.iter().cloned());
        self.c.extend(other.c.iter().cloned());
        self.cxx.extend(other.cxx.iter().cloned());
        self.asm.extend(other.asm.iter().cloned());
    }

    pub fn combined(parts: &[&Self]) -> Self {
        let mut merged = Self::default();
        for part in parts {
            merged.extend(part);
        }
        merged
    }

    /// Effective extra flags for a source file path.
    pub fn for_source(&self, source: &Path) -> Vec<String> {
        let mut flags = self.common.clone();
        let ext = source
            .extension()
            .unwrap_or_default()
            .to_string_lossy()
            .to_lowercase();
        match ext.as_str() {
            "c" => flags.extend(self.c.iter().cloned()),
            "s" | "sx" | "asm" | "spp" => flags.extend(self.asm.iter().cloned()),
            _ => flags.extend(self.cxx.iter().cloned()),
        }
        flags
    }

    /// Flatten all scopes for diagnostics / legacy code paths.
    pub fn flatten(&self) -> Vec<String> {
        let mut flags = self.common.clone();
        flags.extend(self.c.iter().cloned());
        flags.extend(self.cxx.iter().cloned());
        flags.extend(self.asm.iter().cloned());
        flags
    }
}

/// Link-time additions resolved from script mutations.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LinkExtraFlags {
    pub flags: Vec<String>,
    pub libs: Vec<String>,
}

impl LinkExtraFlags {
    pub fn is_empty(&self) -> bool {
        self.flags.is_empty() && self.libs.is_empty()
    }

    pub fn extend(&mut self, other: &Self) {
        self.flags.extend(other.flags.iter().cloned());
        self.libs.extend(other.libs.iter().cloned());
    }
}

/// Native replayable build overlay extracted from extra scripts.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BuildOverlay {
    /// Mutations that apply to the global construction environment (`env`).
    pub global_compile: LanguageExtraFlags,
    /// Mutations that apply only to project sources (`projenv`).
    pub project_compile: LanguageExtraFlags,
    /// Additional link-time arguments.
    pub link: LinkExtraFlags,
    /// User-facing notes emitted by the runtime (e.g. ignored no-op actions).
    pub notes: Vec<String>,
    /// Records the lite-SCons harness's effectful captures: `Execute` runs,
    /// generated files, recorded pre/post actions, middleware, custom
    /// targets, and unmapped builder calls. Optional (rather than always
    /// present) because builds without `extra_scripts` short-circuit
    /// before invoking the harness; populated whenever at least one
    /// script ran. See FastLED/fbuild#553.
    pub lite_scons_records: Option<LiteSconsRecords>,
}

impl BuildOverlay {
    pub fn is_empty(&self) -> bool {
        self.global_compile.is_empty()
            && self.project_compile.is_empty()
            && self.link.is_empty()
            && self.notes.is_empty()
            && self.lite_scons_records.is_none()
    }
}

/// Captures from the lite-SCons harness — exists in parallel to the
/// flag-scope overlay because these records describe side-effects and
/// deferred actions that fbuild's native compile/link/deploy pipeline
/// has to consume separately. See FastLED/fbuild#553.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Deserialize)]
#[serde(default)]
pub struct LiteSconsRecords {
    /// Each effectful `env.Execute(action)` call. `kind` is `"callable"`
    /// or `"command"`.
    pub executed_actions: Vec<serde_json::Value>,
    /// Files materialised by `Execute` callables (mtime > pre-script
    /// snapshot, or new file). fbuild treats these as build inputs.
    pub generated_files: Vec<serde_json::Value>,
    /// `env.AddPreAction(target, action)` records, with the unresolved
    /// target template (e.g. `"$BUILD_DIR/$PROGNAME$PROGSUFFIX"`).
    pub recorded_pre_actions: Vec<serde_json::Value>,
    /// `env.AddPostAction(target, action)` records.
    pub recorded_post_actions: Vec<serde_json::Value>,
    /// `env.AddCustomTarget(name, deps, actions, **kwargs)` records.
    pub custom_targets: Vec<serde_json::Value>,
    /// `env.AddBuildMiddleware(callback, regex=None)` records.
    pub middleware: Vec<serde_json::Value>,
    /// Builder invocations (e.g. `env.MergeFlashImage(...)`). fbuild maps
    /// known builder names to native ops; unknown names surface as a
    /// targeted "needs `--platformio` for builder X" error at the call site.
    pub builder_calls: Vec<serde_json::Value>,
    /// Tracebacks from script exceptions that the harness swallowed.
    pub errors: Vec<String>,
}

/// Serializable form returned by the Python script runtime.
#[derive(Debug, Clone, serde::Deserialize)]
pub(crate) struct ScriptRuntimeResult {
    pub env: ScriptScopeState,
    pub projenv: ScriptScopeState,
    #[serde(default)]
    pub notes: Vec<String>,
    #[serde(default)]
    pub unsupported: Vec<String>,
    /// Lite-SCons-only extension. Always absent when the MockEnv harness ran.
    #[serde(default)]
    pub lite_scons_records: Option<LiteSconsRecords>,
}

#[derive(Debug, Clone, Default, serde::Deserialize)]
pub(crate) struct ScriptScopeState {
    #[serde(default)]
    pub cppdefines: Vec<serde_json::Value>,
    #[serde(default)]
    pub cpppath: Vec<serde_json::Value>,
    #[serde(default)]
    pub ccflags: Vec<serde_json::Value>,
    #[serde(default)]
    pub cflags: Vec<serde_json::Value>,
    #[serde(default)]
    pub cxxflags: Vec<serde_json::Value>,
    #[serde(default)]
    pub asflags: Vec<serde_json::Value>,
    #[serde(default)]
    pub linkflags: Vec<serde_json::Value>,
    #[serde(default)]
    pub libpath: Vec<serde_json::Value>,
    #[serde(default)]
    pub libs: Vec<serde_json::Value>,
}

pub(crate) fn values_to_args(
    values: &[serde_json::Value],
    project_dir: &Path,
) -> fbuild_core::Result<Vec<String>> {
    let mut args = Vec::new();
    for value in values {
        append_value_args(value, project_dir, &mut args)?;
    }
    Ok(args)
}

fn append_value_args(
    value: &serde_json::Value,
    project_dir: &Path,
    args: &mut Vec<String>,
) -> fbuild_core::Result<()> {
    match value {
        serde_json::Value::String(s) => args.push(s.clone()),
        serde_json::Value::Number(n) => args.push(n.to_string()),
        serde_json::Value::Bool(b) => args.push(if *b { "1" } else { "0" }.to_string()),
        serde_json::Value::Array(items) => {
            for item in items {
                append_value_args(item, project_dir, args)?;
            }
        }
        serde_json::Value::Object(map) => {
            if let Some(kind) = map.get("kind").and_then(|v| v.as_str()) {
                match kind {
                    "path" => {
                        let raw = map.get("value").and_then(|v| v.as_str()).ok_or_else(|| {
                            fbuild_core::FbuildError::BuildFailed(
                                "invalid script runtime path entry".to_string(),
                            )
                        })?;
                        let path = absolutize_if_relative(project_dir, raw);
                        args.push(path.to_string_lossy().to_string());
                    }
                    "kv" => {
                        let key = map.get("key").and_then(|v| v.as_str()).ok_or_else(|| {
                            fbuild_core::FbuildError::BuildFailed(
                                "invalid script runtime kv entry".to_string(),
                            )
                        })?;
                        let value = map.get("value").ok_or_else(|| {
                            fbuild_core::FbuildError::BuildFailed(
                                "invalid script runtime kv value".to_string(),
                            )
                        })?;
                        args.push(match value {
                            serde_json::Value::String(s) => format!("{key}={s}"),
                            serde_json::Value::Number(n) => format!("{key}={n}"),
                            serde_json::Value::Bool(b) => {
                                format!("{key}={}", if *b { "1" } else { "0" })
                            }
                            _ => {
                                return Err(fbuild_core::FbuildError::BuildFailed(
                                    "unsupported script runtime kv value".to_string(),
                                ))
                            }
                        });
                    }
                    _ => {
                        return Err(fbuild_core::FbuildError::BuildFailed(format!(
                            "unsupported script runtime entry kind '{}'",
                            kind
                        )))
                    }
                }
            }
        }
        serde_json::Value::Null => {}
    }
    Ok(())
}

pub(crate) fn absolutize_if_relative(project_dir: &Path, raw: &str) -> PathBuf {
    let path = PathBuf::from(raw);
    if path.is_absolute() {
        path
    } else {
        project_dir.join(path)
    }
}

/// Apply user `build_flags` from `platformio.ini` onto a base compiler flag set.
///
/// Matches PlatformIO behavior: user flags are appended to the base, but
/// `-std=` flags replace the existing standard (instead of stacking), and
/// `-D<NAME>` / `-D<NAME>=<value>` flags are deduplicated by macro name so a
/// later override drops the earlier value (which would otherwise produce a GCC
/// redefinition warning).
pub(crate) fn apply_user_flags(base_flags: &[String], user_flags: &[String]) -> Vec<String> {
    let mut result = base_flags.to_vec();
    for flag in user_flags {
        if flag.starts_with("-std=") {
            result.retain(|f| !f.starts_with("-std="));
        } else if let Some(define_name) = define_flag_name(flag) {
            result.retain(|f| define_flag_name(f) != Some(define_name));
        }
        result.push(flag.clone());
    }
    result
}

/// Apply a [`LanguageExtraFlags`] overlay onto a base flag set, picking the
/// language-specific scope from `probe_name`'s extension (`dummy.c` for C,
/// `dummy.cpp` for C++).
pub(crate) fn apply_overlay_flags(
    base_flags: &[String],
    overlay: &LanguageExtraFlags,
    probe_name: &str,
) -> Vec<String> {
    apply_user_flags(base_flags, &overlay.for_source(Path::new(probe_name)))
}

fn define_flag_name(flag: &str) -> Option<&str> {
    let define = flag.strip_prefix("-D")?;
    let name = define
        .split_once('=')
        .map_or(define, |(name, _)| name)
        .trim();
    if name.is_empty() {
        None
    } else {
        Some(name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_apply_user_flags_replaces_std_flag() {
        let base = vec!["-Os".to_string(), "-std=gnu++2b".to_string()];
        let user = vec!["-std=gnu++20".to_string()];

        let result = apply_user_flags(&base, &user);

        assert_eq!(result, vec!["-Os", "-std=gnu++20"]);
    }

    #[test]
    fn test_apply_user_flags_replaces_define_with_same_name() {
        let base = vec![
            r#"-DIDF_VER=\"v5.5.1-710-g8410210c9a\""#.to_string(),
            r#"-DESP_MDNS_VERSION_NUMBER=\"1.9.0\""#.to_string(),
            "-Os".to_string(),
        ];
        let user = vec![
            r#"-DESP_MDNS_VERSION_NUMBER=\"1.9.1\""#.to_string(),
            r#"-DIDF_VER=\"v5.5.2-729-g87912cd291\""#.to_string(),
        ];

        let result = apply_user_flags(&base, &user);

        assert_eq!(
            result,
            vec![
                "-Os",
                r#"-DESP_MDNS_VERSION_NUMBER=\"1.9.1\""#,
                r#"-DIDF_VER=\"v5.5.2-729-g87912cd291\""#,
            ]
        );
    }

    #[test]
    fn test_apply_user_flags_replaces_bare_define_with_value_define() {
        let base = vec!["-DFOO".to_string(), "-DBAR=1".to_string()];
        let user = vec!["-DFOO=2".to_string()];

        let result = apply_user_flags(&base, &user);

        assert_eq!(result, vec!["-DBAR=1", "-DFOO=2"]);
    }

    #[test]
    fn test_apply_user_flags_replaces_existing_define_by_key() {
        let merged = apply_user_flags(
            &[r#"-DIDF_VER=\"old\""#.to_string(), "-O2".to_string()],
            &[r#"-DIDF_VER=\"new\""#.to_string()],
        );
        assert_eq!(
            merged,
            vec![r#"-O2"#.to_string(), r#"-DIDF_VER=\"new\""#.to_string()]
        );
    }

    #[test]
    fn test_apply_user_flags_keeps_last_user_define() {
        let merged = apply_user_flags(
            &[],
            &[
                r#"-DMBEDTLS_CONFIG_FILE=\"a.h\""#.to_string(),
                r#"-DMBEDTLS_CONFIG_FILE=\"b.h\""#.to_string(),
            ],
        );
        assert_eq!(merged, vec![r#"-DMBEDTLS_CONFIG_FILE=\"b.h\""#.to_string()]);
    }

    #[test]
    fn test_apply_overlay_flags_uses_c_scope_for_c_source() {
        let base = vec!["-Os".to_string()];
        let overlay = LanguageExtraFlags {
            common: vec!["-DCOMMON=1".to_string()],
            c: vec!["-DONLY_C=1".to_string()],
            cxx: vec!["-DONLY_CXX=1".to_string()],
            asm: vec![],
        };

        let c_out = apply_overlay_flags(&base, &overlay, "dummy.c");
        let cpp_out = apply_overlay_flags(&base, &overlay, "dummy.cpp");

        assert!(c_out.contains(&"-DCOMMON=1".to_string()));
        assert!(c_out.contains(&"-DONLY_C=1".to_string()));
        assert!(!c_out.contains(&"-DONLY_CXX=1".to_string()));

        assert!(cpp_out.contains(&"-DCOMMON=1".to_string()));
        assert!(cpp_out.contains(&"-DONLY_CXX=1".to_string()));
        assert!(!cpp_out.contains(&"-DONLY_C=1".to_string()));
    }
}
