//! Unit tests for `linker.rs` helpers (ELF freshness, archive manifests,
//! linker scripts), moved out of `linker.rs` to keep it under 1000 lines.

use super::*;

fn set_mtime(path: &Path, time: std::time::SystemTime) {
    std::fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(time)
        .unwrap();
}

fn size(total_flash: u64, total_ram: u64) -> SizeInfo {
    SizeInfo {
        text: total_flash,
        data: 0,
        bss: total_ram,
        total_flash,
        total_ram,
        max_flash: Some(32256),
        max_ram: Some(2048),
        ram_warning: None,
    }
}

/// FastLED/fbuild#1409: an Uno image at 135% flash linked and exited 0.
#[test]
fn flash_overflow_fails_the_link() {
    let err = enforce_size_limits(Some(&size(43662, 100)), false).unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("program size (43662 bytes) is greater than maximum allowed (32256"),
        "{msg}"
    );
}

#[test]
fn ram_overflow_fails_only_where_fatal() {
    let over = size(1000, 42184);
    assert!(enforce_size_limits(Some(&over), false).is_ok());
    let msg = enforce_size_limits(Some(&over), true)
        .unwrap_err()
        .to_string();
    assert!(msg.contains("data size (42184 bytes)"), "{msg}");
}

#[test]
fn fitting_or_unmeasured_image_passes() {
    assert!(enforce_size_limits(Some(&size(32256, 2048)), true).is_ok());
    assert!(enforce_size_limits(None, true).is_ok());
}

#[test]
fn archive_is_current_requires_the_same_objects_and_no_newer_object() {
    let tmp = tempfile::TempDir::new().unwrap();
    let a = tmp.path().join("a.o");
    let b = tmp.path().join("b.o");
    std::fs::write(&a, b"a").unwrap();
    std::fs::write(&b, b"b").unwrap();
    let archive = tmp.path().join("libcore.a");
    let objects = vec![a.clone(), b.clone()];
    assert!(
        !LinkerBase::archive_is_current(&archive, &objects),
        "no archive yet"
    );

    // What `archive_if_stale` leaves behind, without running `ar`.
    std::fs::write(&archive, b"!<arch>\n").unwrap();
    std::fs::write(archive_manifest_path(&archive), archive_manifest(&objects)).unwrap();
    let past = std::time::SystemTime::now() - std::time::Duration::from_secs(60);
    set_mtime(&a, past);
    set_mtime(&b, past);
    assert!(LinkerBase::archive_is_current(&archive, &objects));

    assert!(
        !LinkerBase::archive_is_current(&archive, std::slice::from_ref(&a)),
        "an object left the build, so its member is stale"
    );
    set_mtime(
        &b,
        std::time::SystemTime::now() + std::time::Duration::from_secs(60),
    );
    assert!(
        !LinkerBase::archive_is_current(&archive, &objects),
        "an object was rebuilt after the archive"
    );
}

/// Absolute path for the running platform (`/x` is *not* absolute on
/// Windows — it has a root but no drive prefix).
fn abs(tail: &str) -> PathBuf {
    if fbuild_core::platform::host::is_windows() {
        PathBuf::from(format!("C:\\{}", tail.replace('/', "\\")))
    } else {
        PathBuf::from(format!("/{tail}"))
    }
}

/// The link must not run in the daemon's inherited cwd: collect2 /
/// lto-wrapper write scratch files relative to it, which put a stray
/// 0-byte `-r` in the user's repo root on every clean build.
/// FastLED/fbuild#1267, FastLED/FastLED#3867.
#[test]
fn link_cwd_is_absolute_output_dir_when_all_paths_absolute() {
    let rel = format!(
        "proj/{}/{}/release",
        fbuild_paths::FBUILD_DIR_NAME,
        fbuild_paths::BUILD_DIR_NAME
    );
    let out = abs(&rel);
    let paths = [abs(&format!("{rel}/sketch.o")), abs("sdk/ld")];
    assert_eq!(
        link_cwd_for(&out, &paths),
        Some(out.as_path()),
        "absolute output dir must become the link cwd so linker scratch \
         files stay in the build tree"
    );
}

/// No path arguments at all is still safe — `-o` lives under `output_dir`.
#[test]
fn link_cwd_is_absolute_output_dir_with_no_path_args() {
    let out = abs("proj/build");
    let none: [PathBuf; 0] = [];
    assert_eq!(link_cwd_for(&out, &none), Some(out.as_path()));
}

/// A relative output dir keeps the historical inherited-cwd behaviour:
/// moving cwd there would also relocate the relative `-o` path.
#[test]
fn link_cwd_is_unset_for_relative_output_dir() {
    let none: [PathBuf; 0] = [];
    assert_eq!(link_cwd_for(Path::new("build/release"), &none), None);
}

/// A single relative object / script / search dir is enough to disable the
/// cwd change: it would be re-resolved against the new working directory.
#[test]
fn link_cwd_is_unset_when_any_path_arg_is_relative() {
    let out = abs("proj/build");
    let paths = [abs("proj/build/sketch.o"), PathBuf::from("variant/lib.a")];
    assert_eq!(link_cwd_for(&out, &paths), None);
}

#[test]
fn test_collect_objects_empty() {
    let tmp = tempfile::TempDir::new().unwrap();
    let objects = LinkerBase::collect_objects(tmp.path());
    assert!(objects.is_empty());
}

#[test]
fn test_collect_objects() {
    let tmp = tempfile::TempDir::new().unwrap();
    std::fs::write(tmp.path().join("main.o"), "").unwrap();
    std::fs::write(tmp.path().join("helper.o"), "").unwrap();
    std::fs::write(tmp.path().join("readme.txt"), "").unwrap();

    let objects = LinkerBase::collect_objects(tmp.path());
    assert_eq!(objects.len(), 2);
    assert!(objects.iter().all(|p| p.extension().unwrap() == "o"));
}

#[test]
fn test_linker_scripts_default_is_empty() {
    let ls = LinkerScripts::new();
    assert!(ls.is_empty());
    assert!(ls.to_args().is_empty());
}

#[test]
fn test_linker_scripts_single() {
    let ls = LinkerScripts::single(PathBuf::from("/sdk/ld"), "eagle.flash.4m1m.ld");
    assert!(!ls.is_empty());
    assert_eq!(ls.search_dirs, vec![PathBuf::from("/sdk/ld")]);
    assert_eq!(ls.scripts, vec!["eagle.flash.4m1m.ld"]);
    assert_eq!(ls.to_args(), vec!["-L/sdk/ld", "-Teagle.flash.4m1m.ld"]);
}

#[test]
fn test_linker_scripts_from_raw_flags_concatenated() {
    let flags = vec![
        "-L/sdk/ld".to_string(),
        "-Tmemory.ld".to_string(),
        "-Tsections.ld".to_string(),
    ];
    let ls = LinkerScripts::from_raw_flags(&flags);
    assert_eq!(ls.search_dirs, vec![PathBuf::from("/sdk/ld")]);
    assert_eq!(ls.scripts, vec!["memory.ld", "sections.ld"]);
    assert_eq!(
        ls.to_args(),
        vec!["-L/sdk/ld", "-Tmemory.ld", "-Tsections.ld"]
    );
}

#[test]
fn test_linker_scripts_from_raw_flags_separated() {
    let flags = vec![
        "-L/sdk/ld".to_string(),
        "-T".to_string(),
        "memory.ld".to_string(),
    ];
    let ls = LinkerScripts::from_raw_flags(&flags);
    assert_eq!(ls.scripts, vec!["memory.ld"]);
}

#[test]
fn test_linker_scripts_from_raw_flags_empty() {
    let ls = LinkerScripts::from_raw_flags(&[]);
    assert!(ls.is_empty());
}

#[test]
fn test_linker_scripts_to_args_order() {
    let ls = LinkerScripts {
        search_dirs: vec![PathBuf::from("/a"), PathBuf::from("/b")],
        scripts: vec!["x.ld".to_string(), "y.ld".to_string()],
    };
    assert_eq!(ls.to_args(), vec!["-L/a", "-L/b", "-Tx.ld", "-Ty.ld"]);
}

#[test]
fn test_nm_path_from_size_path_arm() {
    let size = PathBuf::from("/toolchain/bin/arm-none-eabi-size");
    let nm = LinkerBase::nm_path_from_size_path(&size);
    assert_eq!(nm, PathBuf::from("/toolchain/bin/arm-none-eabi-nm"));
}

#[test]
fn test_nm_path_from_size_path_avr() {
    let size = PathBuf::from("/toolchain/bin/avr-size");
    let nm = LinkerBase::nm_path_from_size_path(&size);
    assert_eq!(nm, PathBuf::from("/toolchain/bin/avr-nm"));
}

#[test]
fn test_nm_path_from_size_path_xtensa() {
    let size = PathBuf::from("/toolchain/bin/xtensa-esp32-elf-size");
    let nm = LinkerBase::nm_path_from_size_path(&size);
    assert_eq!(nm, PathBuf::from("/toolchain/bin/xtensa-esp32-elf-nm"));
}

#[test]
fn test_nm_path_from_size_path_with_exe() {
    let size = PathBuf::from("C:/toolchain/bin/arm-none-eabi-size.exe");
    let nm = LinkerBase::nm_path_from_size_path(&size);
    assert_eq!(nm, PathBuf::from("C:/toolchain/bin/arm-none-eabi-nm.exe"));
}

/// APE host tools carry the `.com` suffix even on Linux (#1633): the sibling
/// `nm` must keep the same suffix or `size`/`nm` disagree about the tool.
#[test]
fn test_nm_path_from_size_path_with_ape_com() {
    let size = PathBuf::from("/toolchain/bin/arm-none-eabi-size.com");
    let nm = LinkerBase::nm_path_from_size_path(&size);
    assert_eq!(nm, PathBuf::from("/toolchain/bin/arm-none-eabi-nm.com"));
}
