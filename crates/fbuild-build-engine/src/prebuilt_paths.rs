//! Named, lossless path roles for reproducibly compiled framework inputs.
use fbuild_core::path::NormalizedPath;
use std::path::Path;

#[derive(Clone)]
pub struct PathMapping {
    pub root: NormalizedPath,
    pub prefix: String,
}
impl PathMapping {
    pub fn new(root: &Path, prefix: &str) -> Self {
        Self {
            root: NormalizedPath::new(root),
            prefix: prefix.into(),
        }
    }
}

pub fn mapped_path(path: &Path, cwd: &Path, mappings: &[PathMapping]) -> String {
    let path = if path.is_absolute() {
        NormalizedPath::new(path)
    } else {
        NormalizedPath::new(cwd).join(path)
    };
    mappings
        .iter()
        .filter_map(|mapping| {
            path.relative_to(&mapping.root)
                .map(|relative| (mapping, relative))
        })
        .next_back()
        .map_or_else(
            || path.display_slash(),
            |(mapping, relative)| {
                if relative.as_path() == Path::new("") || relative.as_path() == Path::new(".") {
                    mapping.prefix.clone()
                } else {
                    format!("{}/{}", mapping.prefix, relative.display_slash())
                }
            },
        )
}

/// Canonicalize only typed path operands. Defines and all other flag bytes
/// retain their original order and spelling; output names are logical TU IDs.
pub fn canonical_argv(
    args: &[String],
    cwd: &Path,
    mappings: &[PathMapping],
    ordinal: usize,
) -> Option<Vec<String>> {
    let mut output = Vec::new();
    let mut operand = None;
    for arg in args {
        if let Some(kind) = operand.take() {
            output.push(if kind == "-o" {
                format!("/fbuild/obj/{ordinal}.o")
            } else {
                mapped_path(Path::new(arg), cwd, mappings)
            });
            continue;
        }
        if arg.starts_with('@')
            || matches!(
                arg.as_str(),
                "-include" | "-imacros" | "-frecord-gcc-switches" | "-grecord-gcc-switches"
            )
            || arg.starts_with("-include")
            || arg.starts_with("-imacros")
            || arg.starts_with("-fplugin")
        {
            return None;
        }
        if matches!(
            arg.as_str(),
            "-c" | "-o" | "-I" | "-isystem" | "-iquote" | "-idirafter" | "-isysroot"
        ) {
            operand = Some(arg.as_str());
            output.push(arg.clone());
            continue;
        }
        if let Some((prefix, old, new)) = prefix_map(arg) {
            output.push(format!(
                "{prefix}{}={new}",
                mapped_path(Path::new(old), cwd, mappings)
            ));
            continue;
        }
        if let Some((prefix, path)) = ["-I", "-isystem", "-iquote", "-idirafter", "--sysroot="]
            .iter()
            .find_map(|prefix| {
                arg.strip_prefix(prefix)
                    .filter(|p| !p.is_empty())
                    .map(|path| (*prefix, path))
            })
        {
            output.push(format!(
                "{prefix}{}",
                mapped_path(Path::new(path), cwd, mappings)
            ));
            continue;
        }
        output.push(if Path::new(arg).is_absolute() {
            mapped_path(Path::new(arg), cwd, mappings)
        } else {
            arg.clone()
        });
    }
    operand.is_none().then_some(output)
}

fn prefix_map(arg: &str) -> Option<(&str, &str, &str)> {
    for prefix in [
        "-ffile-prefix-map=",
        "-fdebug-prefix-map=",
        "-fmacro-prefix-map=",
    ] {
        if let Some(rest) = arg.strip_prefix(prefix) {
            let (old, new) = rest.split_once('=')?;
            return Some((prefix, old, new));
        }
    }
    None
}

/// V1 fails closed when the compiler's physical CWD differs from mapped roots.
pub async fn unambiguous_roots(mappings: &[PathMapping]) -> bool {
    for mapping in mappings {
        if !fbuild_core::path::canonicalize_existing(&mapping.root)
            .await
            .is_ok_and(|physical| physical == mapping.root)
        {
            return false;
        }
    }
    true
}
