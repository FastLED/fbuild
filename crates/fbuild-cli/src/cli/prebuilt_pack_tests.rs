//! Wiring tests for the hidden artifact builder (#1882): the command packs a
//! real tree, writes the entry the client reads, refuses what cannot be served,
//! and stays out of `--help`.
use super::{PackArgs, run_prebuilt_pack};
use clap::CommandFactory as _;
use fbuild_core::path::NormalizedPath;
use serde_json::Value;
use tempfile::TempDir;

const FP: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

/// A source tree, the objects a real compile would have produced for it (named
/// by the same `CompilerBase::object_path` the client hydrates to), and the unit
/// manifest file the command reads.
fn objects(dir: &TempDir) -> (NormalizedPath, String) {
    let root = NormalizedPath::new(dir.path().join("objects"));
    std::fs::create_dir_all(root.as_path()).unwrap();
    let sources = NormalizedPath::new(dir.path().join("framework/cores/esp32"));
    std::fs::create_dir_all(sources.as_path()).unwrap();
    let mut manifest = String::new();
    for (unit, name) in [
        ("cores/esp32/main.cpp", "main.cpp"),
        ("cores/esp32/hal.c", "hal.c"),
    ] {
        let source = sources.join(name);
        std::fs::write(source.as_path(), "// unit\n").unwrap();
        let object =
            fbuild_build::compiler::CompilerBase::object_path(source.as_path(), root.as_path());
        std::fs::write(&object, b"compiled core object").unwrap();
        manifest.push_str(&format!("{unit}\t{}\n", source.as_path().display()));
    }
    let units = dir.path().join("units.tsv");
    std::fs::write(&units, manifest).unwrap();
    (root, units.display().to_string())
}

fn args(root: &std::path::Path, out: &std::path::Path, units: &str) -> PackArgs {
    PackArgs {
        objects: root.display().to_string(),
        out: out.display().to_string(),
        vendor: "espressif".into(),
        product: None,
        channel: "release".into(),
        reference: "3.3.12".into(),
        chip: "esp32dev".into(),
        framework: "arduino-esp32".into(),
        fingerprint: FP.into(),
        units: units.to_string(),
        arch: "esp32".into(),
        abi: "gcc@14.2.0".into(),
        base_url: "https://bucket.example".into(),
        min_client_version: 1,
    }
}

#[test]
fn the_command_writes_an_archive_and_the_entry_the_client_reads() {
    let dir = TempDir::new().unwrap();
    let (root, units) = objects(&dir);
    let out = dir.path().join("out");
    run_prebuilt_pack(args(&root, &out, &units)).unwrap();

    let entry: Value =
        serde_json::from_slice(&std::fs::read(out.join("release.json")).unwrap()).unwrap();
    let shard: Value = serde_json::from_slice(
        &std::fs::read(out.join("prebuilt-arduino-esp32.catalog.json")).unwrap(),
    )
    .unwrap();
    let platform = &entry["platforms"][0];
    let filename = platform["asset"]["filename"].as_str().unwrap();
    assert_eq!(
        filename,
        format!(
            "arduino-esp32-core-3.3.12-esp32dev.{}.tar.zst",
            &platform["asset"]["sha256"].as_str().unwrap()[..16]
        )
    );
    assert_eq!(platform["variant"]["flavor"], format!("fp-{}", &FP[..16]));
    assert_eq!(platform["asset"]["provides"][0], format!("fbuild-fp:{FP}"));
    assert!(
        std::fs::read(out.join(filename)).is_ok(),
        "the archive sits next to its entry"
    );
    assert_eq!(shard["kind"], "Catalog");
    assert_eq!(shard["tool"], "prebuilt-arduino-esp32");
    assert_eq!(shard["channels"]["3.3.12"], "3.3.12");
    assert_eq!(shard["releases"][0]["version"], "3.3.12");
}

#[test]
fn a_request_that_cannot_be_served_fails_without_writing_output() {
    let dir = TempDir::new().unwrap();
    let (root, units) = objects(&dir);
    let out = dir.path().join("out");
    let mut bad = args(&root, &out, &units);
    bad.fingerprint = "deadbeef".into();
    let error = run_prebuilt_pack(bad).unwrap_err();
    assert!(error.to_string().contains("fingerprint"), "{error}");
    assert!(!out.exists());
}

#[test]
fn the_builder_is_hidden_from_help_and_parses_as_a_command() {
    let command = crate::cli::args::Cli::command();
    let help = command.clone().render_long_help().to_string();
    assert!(
        !help.contains("__prebuilt-pack"),
        "the publisher tool must not appear in help"
    );
    let parsed = command
        .try_get_matches_from([
            "fbuild",
            "__prebuilt-pack",
            "--objects",
            "objects",
            "--out",
            "out",
            "--reference",
            "3.3.12",
            "--chip",
            "esp32dev",
            "--fingerprint",
            FP,
            "--units",
            "units.tsv",
            "--arch",
            "esp32",
            "--abi",
            "gcc@14.2.0",
            "--base-url",
            "https://bucket.example",
        ])
        .expect("the hidden command still parses");
    assert!(parsed.subcommand_matches("__prebuilt-pack").is_some());
}

/// The archive carries `objects.json`, naming the object file the publisher's
/// own compiler derives for each source — the mapping the client hydrates with.
#[test]
fn the_archive_carries_the_unit_manifest_the_client_reads() {
    let dir = TempDir::new().unwrap();
    let (root, units) = objects(&dir);
    let out = dir.path().join("out");
    run_prebuilt_pack(args(&root, &out, &units)).unwrap();
    let entry: Value =
        serde_json::from_slice(&std::fs::read(out.join("release.json")).unwrap()).unwrap();
    let filename = entry["platforms"][0]["asset"]["filename"]
        .as_str()
        .unwrap()
        .to_string();
    let bytes = std::fs::read(out.join(filename)).unwrap();
    let mut archive = tar::Archive::new(zstd::Decoder::new(&bytes[..]).unwrap());
    let manifest = archive
        .entries()
        .unwrap()
        .filter_map(Result::ok)
        .find(|item| {
            item.path().unwrap().to_string_lossy()
                == fbuild_packages_fetch::prebuilt::OBJECTS_MANIFEST
        })
        .expect("the archive carries the manifest");
    let body: Value = serde_json::from_reader(manifest).unwrap();
    assert_eq!(
        body["schema"],
        fbuild_packages_fetch::prebuilt::MANIFEST_SCHEMA
    );
    let named = &body["units"]["cores/esp32/main.cpp"];
    assert!(
        named.as_str().unwrap().starts_with("main_"),
        "the object name is the compiler's own, not a guess: {named}"
    );
    assert!(
        named.as_str().unwrap().ends_with(".cpp.o"),
        "the source extension is preserved: {named}"
    );
}

/// A unit line without a tab, an empty manifest, and a source that does not
/// produce an object in the tree are all refused before anything is written.
#[test]
fn an_unusable_unit_manifest_fails_without_writing_output() {
    let dir = TempDir::new().unwrap();
    let (root, _) = objects(&dir);
    let out = dir.path().join("out");

    let unterminated = dir.path().join("no-tab.tsv");
    std::fs::write(&unterminated, "cores/esp32/main.cpp\n").unwrap();
    let error = run_prebuilt_pack(args(&root, &out, &unterminated.display().to_string()))
        .unwrap_err()
        .to_string();
    assert!(error.contains("expected"), "{error}");

    let empty = dir.path().join("empty.tsv");
    std::fs::write(&empty, "# nothing here\n\n").unwrap();
    let error = run_prebuilt_pack(args(&root, &out, &empty.display().to_string()))
        .unwrap_err()
        .to_string();
    assert!(error.contains("names no unit"), "{error}");

    // A source the compile never produced an object for: the derived name is not
    // in the tree, so the archive would advertise an unservable file.
    let absent_source = dir.path().join("framework/cores/esp32/gone.cpp");
    std::fs::write(&absent_source, "// never compiled\n").unwrap();
    let absent = dir.path().join("absent.tsv");
    std::fs::write(
        &absent,
        format!("cores/esp32/gone.cpp\t{}\n", absent_source.display()),
    )
    .unwrap();
    let error = run_prebuilt_pack(args(&root, &out, &absent.display().to_string()))
        .unwrap_err()
        .to_string();
    assert!(error.contains("does not carry"), "{error}");
    assert!(!out.exists(), "nothing was written");
}
