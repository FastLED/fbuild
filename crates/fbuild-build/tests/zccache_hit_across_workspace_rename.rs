use std::path::{Path, PathBuf};
use std::process::Command;
use std::{env, fs};

use fbuild_build::compiler::compile_source;

const FAKE_ZCCACHE: &str = r#"
use std::env;
use std::fs;
use std::io::Write;
use std::path::{Component, Path, PathBuf};

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    if args.len() < 2 || args[0] != "wrap" {
        eprintln!("usage: fake-zccache wrap <compiler> <args...>");
        std::process::exit(2);
    }

    let cwd = env::current_dir().unwrap();
    let expanded = expand_response_files(&args[2..]);
    let source = find_source(&expanded, &cwd).expect("source file");
    let output = find_output(&expanded, &cwd).expect("output file");
    let includes = find_includes(&expanded, &cwd);

    let key = cache_key(&cwd, &source, &includes);
    let key_hash = stable_hash(key.as_bytes());
    let cache_dir = PathBuf::from(env::var("FBUILD_FAKE_ZCCACHE_CACHE").unwrap());
    let log_path = PathBuf::from(env::var("FBUILD_FAKE_ZCCACHE_LOG").unwrap());
    fs::create_dir_all(&cache_dir).unwrap();
    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent).unwrap();
    }

    let cache_path = cache_dir.join(format!("{key_hash:016x}.o"));
    let mut log = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)
        .unwrap();

    if cache_path.exists() {
        fs::copy(&cache_path, &output).unwrap();
        writeln!(log, "hit cwd={} key={key_hash:016x}", cwd.display()).unwrap();
    } else {
        let object = format!("object\n{}\n", key);
        fs::write(&output, object.as_bytes()).unwrap();
        fs::copy(&output, &cache_path).unwrap();
        writeln!(log, "miss cwd={} key={key_hash:016x}", cwd.display()).unwrap();
    }
}

fn expand_response_files(args: &[String]) -> Vec<String> {
    let mut expanded = Vec::new();
    for arg in args {
        if let Some(path) = arg.strip_prefix('@') {
            let text = fs::read_to_string(path).unwrap();
            expanded.extend(text.split_whitespace().map(unquote));
        } else {
            expanded.push(arg.clone());
        }
    }
    expanded
}

fn unquote(value: &str) -> String {
    value
        .trim_matches('"')
        .trim_matches('\'')
        .to_string()
}

fn find_source(args: &[String], cwd: &Path) -> Option<PathBuf> {
    let mut after_c = false;
    for arg in args {
        if after_c {
            return Some(resolve(arg, cwd));
        }
        after_c = arg == "-c";
    }
    args.iter()
        .find(|arg| !arg.starts_with('-') && is_source(arg))
        .map(|arg| resolve(arg, cwd))
}

fn find_output(args: &[String], cwd: &Path) -> Option<PathBuf> {
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        if arg == "-o" {
            return args.get(i + 1).map(|value| resolve(value, cwd));
        }
        if let Some(value) = arg.strip_prefix("-o") {
            return Some(resolve(value, cwd));
        }
        i += 1;
    }
    None
}

fn find_includes(args: &[String], cwd: &Path) -> Vec<PathBuf> {
    let mut includes = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        if arg == "-I" {
            if let Some(value) = args.get(i + 1) {
                includes.push(resolve(value, cwd));
            }
            i += 2;
            continue;
        }
        if let Some(value) = arg.strip_prefix("-I") {
            includes.push(resolve(value, cwd));
        }
        i += 1;
    }
    includes
}

fn cache_key(cwd: &Path, source: &Path, includes: &[PathBuf]) -> String {
    let mut key = String::new();
    key.push_str("source=");
    key.push_str(&key_path(source, cwd));
    key.push(':');
    key.push_str(&fs::read_to_string(source).unwrap());
    key.push('\n');

    for include in includes {
        key.push_str("include-dir=");
        key.push_str(&key_path(include, cwd));
        key.push('\n');
        let header = include.join("demo.h");
        key.push_str("header=");
        key.push_str(&key_path(&header, cwd));
        key.push(':');
        key.push_str(&fs::read_to_string(header).unwrap());
        key.push('\n');
    }
    key
}

fn key_path(path: &Path, cwd: &Path) -> String {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    };
    let comparable = absolute.strip_prefix(cwd).unwrap_or(&absolute);
    comparable
        .components()
        .filter_map(|component| match component {
            Component::Prefix(prefix) => Some(prefix.as_os_str().to_string_lossy().replace('\\', "/")),
            Component::RootDir | Component::CurDir => None,
            Component::ParentDir => Some("..".to_string()),
            Component::Normal(value) => Some(value.to_string_lossy().replace('\\', "/")),
        })
        .collect::<Vec<_>>()
        .join("/")
}

fn resolve(value: &str, cwd: &Path) -> PathBuf {
    let path = Path::new(value);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    }
}

fn is_source(value: &str) -> bool {
    Path::new(value)
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| matches!(ext, "c" | "cc" | "cpp" | "cxx"))
}

fn stable_hash(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}
"#;

struct CurrentDirGuard {
    original: PathBuf,
}

impl CurrentDirGuard {
    fn set_to(path: &Path) -> Self {
        let original = env::current_dir().unwrap();
        env::set_current_dir(path).unwrap();
        Self { original }
    }
}

impl Drop for CurrentDirGuard {
    fn drop(&mut self) {
        let _ = env::set_current_dir(&self.original);
    }
}

#[test]
fn zccache_hit_across_workspace_rename() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fake_zccache = compile_fake_zccache(tmp.path());
    let fake_compiler = tmp
        .path()
        .join(format!("fake-compiler{}", env::consts::EXE_SUFFIX));
    let cache_dir = tmp.path().join("fake-cache");
    let log_path = tmp.path().join("fake-zccache.log");
    let ws_a = tmp.path().join("workspace-a");
    let ws_b = tmp.path().join("workspace-b");

    create_workspace(&ws_a);
    create_workspace(&ws_b);
    let expected_ws_a = cwd_display_path(&ws_a);
    let expected_ws_b = cwd_display_path(&ws_b);

    let _cwd = CurrentDirGuard::set_to(tmp.path());
    env::set_var("FBUILD_FAKE_ZCCACHE_CACHE", &cache_dir);
    env::set_var("FBUILD_FAKE_ZCCACHE_LOG", &log_path);

    compile_workspace(&ws_a, &fake_compiler, &fake_zccache);
    compile_workspace(&ws_b, &fake_compiler, &fake_zccache);

    let log = fs::read_to_string(&log_path).unwrap();
    let lines: Vec<&str> = log.lines().collect();
    assert_eq!(lines.len(), 2, "unexpected fake zccache log:\n{log}");
    assert!(
        lines[0].starts_with("miss "),
        "first compile should populate the cache:\n{log}"
    );
    assert!(
        lines[1].starts_with("hit "),
        "renamed workspace should reuse the cache entry:\n{log}"
    );
    assert!(
        lines[0].contains(&format!("cwd={expected_ws_a}")),
        "first wrapper CWD should be workspace root:\n{log}"
    );
    assert!(
        lines[1].contains(&format!("cwd={expected_ws_b}")),
        "second wrapper CWD should be workspace root:\n{log}"
    );
}

fn cwd_display_path(path: &Path) -> String {
    let path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let display = path.display().to_string();
    display
        .strip_prefix(r"\\?\")
        .unwrap_or(&display)
        .to_string()
}

fn compile_fake_zccache(root: &Path) -> PathBuf {
    let source = root.join("fake_zccache.rs");
    let exe = root.join(format!("fake-zccache{}", env::consts::EXE_SUFFIX));
    fs::write(&source, FAKE_ZCCACHE).unwrap();

    let rustc = env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
    let status = Command::new(rustc)
        .arg(&source)
        .arg("-o")
        .arg(&exe)
        .status()
        .expect("failed to spawn rustc for fake zccache");
    assert!(status.success(), "failed to compile fake zccache helper");
    exe
}

fn create_workspace(root: &Path) {
    fs::create_dir_all(root.join("src")).unwrap();
    fs::create_dir_all(root.join("include")).unwrap();
    fs::create_dir_all(root.join(".fbuild").join("build")).unwrap();
    fs::write(
        root.join("include").join("demo.h"),
        "#pragma once\ninline int demo() { return 7; }\n",
    )
    .unwrap();
    fs::write(
        root.join("src").join("main.cpp"),
        "#include \"demo.h\"\nint main() { return demo(); }\n",
    )
    .unwrap();
}

fn compile_workspace(root: &Path, compiler: &Path, zccache: &Path) {
    let source = root.join("src").join("main.cpp");
    let output = root.join(".fbuild").join("build").join("main.o");
    let flags = vec![
        "-I".to_string(),
        root.join("include").to_string_lossy().to_string(),
    ];

    let result = compile_source(
        compiler,
        &source,
        &output,
        &flags,
        &[],
        &root.join(".fbuild").join("build").join("tmp"),
        "zccache-rename",
        false,
        Some(zccache),
        &[],
    )
    .unwrap();

    assert!(
        result.success,
        "compile failed: stdout={} stderr={}",
        result.stdout, result.stderr
    );
    assert!(output.exists(), "expected object at {}", output.display());
}
