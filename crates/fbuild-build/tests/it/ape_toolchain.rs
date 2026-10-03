//! A toolchain made entirely of APE (cosmocc) executables, driven the two
//! ways fbuild runs compilers: `run_command` (fbuild's own spawn path) and the
//! embedded zccache service (which spawns the compiler itself).
//!
//! cosmocc's gcc, cc1, as and ld are all APE images, and gcc spawns cc1/as
//! itself, so this exercises nested APE spawns too. Both paths give the
//! compiler a hostile environment (no usable PATH/TMPDIR/HOME; zccache passes
//! an empty env), so only fbuild's own loader support can make it work.
//!
//! Opt-in: point `FBUILD_TEST_COSMOCC` at an unpacked
//! <https://cosmo.zip/pub/cosmocc/cosmocc.zip> (the directory holding `bin/`).

use std::path::{Path, PathBuf};

use fbuild_build::zccache_embedded::FbuildZccacheService;
use zccache::embedded::ShutdownMode;

fn cosmo_gcc() -> PathBuf {
    let root = std::env::var_os("FBUILD_TEST_COSMOCC").expect("FBUILD_TEST_COSMOCC");
    let arch = fbuild_core::platform::host::arch_name();
    let gcc = Path::new(&root)
        .join("bin")
        .join(format!("{arch}-linux-cosmo-gcc"));
    assert!(
        fbuild_core::platform::ape::is_ape_file(&gcc),
        "{} must be an APE image",
        gcc.display()
    );
    gcc
}

/// A packaged-toolchain layout of the APE gcc. Raw cosmocc gcc has no
/// built-in system include dirs (the `cosmocc` driver script passes them), and
/// zccache bypasses its cache when include discovery finds none. A platform
/// package ships its gcc configured, so mirror that: a tree of symlinks into
/// the unpacked cosmocc with the gcc APE image copied in and a GCC `specs` file
/// adding the toolchain's include root.
fn packaged_cosmo_gcc(into: &Path) -> PathBuf {
    let src = cosmo_gcc();
    let root = src.parent().unwrap().parent().unwrap();
    let triple = format!("{}-linux-cosmo", fbuild_core::platform::host::arch_name());
    for entry in std::fs::read_dir(root).unwrap() {
        let name = entry.unwrap().file_name();
        if name != "bin" && name != "lib" {
            link(&root.join(&name), &into.join(&name)).unwrap();
        }
    }
    std::fs::create_dir_all(into.join("bin")).unwrap();
    let gcc = into.join("bin").join(src.file_name().unwrap());
    std::fs::copy(&src, &gcc).unwrap();
    // GCC reads `specs` from lib/gcc/<triple>/<version>/ under its prefix.
    let version = std::fs::read_dir(root.join("libexec/gcc").join(&triple))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .file_name();
    let dest = into.join("lib/gcc").join(&triple).join(&version);
    std::fs::create_dir_all(&dest).unwrap();
    for entry in std::fs::read_dir(root.join("lib/gcc")).unwrap() {
        let entry = entry.unwrap();
        if entry.file_type().unwrap().is_file() {
            link(&entry.path(), &into.join("lib/gcc").join(entry.file_name())).unwrap();
        }
    }
    std::fs::write(
        dest.join("specs"),
        format!("*cpp:\n+ -isystem {}\n\n", root.join("include").display()),
    )
    .unwrap();
    gcc
}

fn link(original: &Path, link: &Path) -> std::io::Result<()> {
    if original.is_dir() {
        fbuild_core::platform::fs::symlink_dir(original, link)
    } else {
        fbuild_core::platform::fs::symlink_file(original, link)
    }
}

fn source(dir: &Path) -> PathBuf {
    let src = dir.join("add.c");
    std::fs::write(&src, "int add(int a, int b) { return a + b; }\n").unwrap();
    src
}

fn compile_args(src: &Path, obj: &Path) -> Vec<String> {
    vec![
        "-nostdinc".into(),
        "-c".into(),
        src.display().to_string(),
        "-o".into(),
        obj.display().to_string(),
    ]
}

fn zccache_args(src: &Path, obj: &Path) -> Vec<String> {
    vec![
        "-c".into(),
        src.display().to_string(),
        "-o".into(),
        obj.display().to_string(),
    ]
}

#[test]
#[ignore = "requires the cosmocc toolchain (~440 MB) via FBUILD_TEST_COSMOCC"]
fn ape_gcc_compiles_through_run_command_in_hostile_env() {
    let gcc = cosmo_gcc();
    let dir = tempfile::tempdir().unwrap();
    let src = source(dir.path());
    let obj = dir.path().join("add.o");
    let mut argv = vec![gcc.display().to_string()];
    argv.extend(compile_args(&src, &obj));
    let argv: Vec<&str> = argv.iter().map(String::as_str).collect();
    let out = fbuild_core::subprocess::run_command_blocking(
        &argv,
        Some(dir.path()),
        Some(&[
            ("PATH", "/nonexistent"),
            ("TMPDIR", "/nonexistent"),
            ("HOME", "/nonexistent"),
        ]),
        None,
    )
    .expect("APE gcc must spawn");
    assert!(
        out.success(),
        "stdout: {}\nstderr: {}",
        out.stdout,
        out.stderr
    );
    assert!(std::fs::read(&obj).unwrap().starts_with(b"\x7fELF"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires the cosmocc toolchain (~440 MB) via FBUILD_TEST_COSMOCC"]
async fn ape_gcc_compiles_through_embedded_zccache_and_hits_cache() {
    let tmp = tempfile::tempdir().unwrap();
    let toolchain = tmp.path().join("toolchain");
    std::fs::create_dir(&toolchain).unwrap();
    let gcc = packaged_cosmo_gcc(&toolchain);
    let svc = FbuildZccacheService::start_in(tmp.path().join("zccache"))
        .await
        .expect("zccache starts");
    let work = tmp.path().join("work");
    std::fs::create_dir(&work).unwrap();
    let src = source(&work);
    let obj = work.join("add.o");
    // The same hermetic env production compiles get (no inherited PATH).
    let mut env = fbuild_core::subprocess::compile_env_for_build(&work).unwrap();
    // A daemon started with a minimal PATH (systemd service, launchd): no
    // coreutils for the APE prologue's own fallback to lean on.
    env.retain(|(k, _)| k != "PATH");
    env.push(("PATH".to_string(), "/nonexistent".to_string()));
    env.push((
        "ZCCACHE_WORKTREE_ROOT".to_string(),
        work.display().to_string(),
    ));
    for round in 0..2 {
        let _ = std::fs::remove_file(&obj);
        let out = svc
            .compile(&gcc, zccache_args(&src, &obj), work.clone(), env.clone())
            .await
            .expect("zccache compile");
        assert_eq!(
            out.exit_code,
            0,
            "round {round} stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(std::fs::read(&obj).unwrap().starts_with(b"\x7fELF"));
        assert_eq!(
            out.cached,
            round == 1,
            "round {round}: cold miss, then warm hit"
        );
        svc.flush().await.expect("flush into the cache");
    }
    svc.shutdown(ShutdownMode::Graceful).await.unwrap();
}
