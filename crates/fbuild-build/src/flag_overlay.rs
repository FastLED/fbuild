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
}

impl BuildOverlay {
    pub fn is_empty(&self) -> bool {
        self.global_compile.is_empty()
            && self.project_compile.is_empty()
            && self.link.is_empty()
            && self.notes.is_empty()
    }
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
