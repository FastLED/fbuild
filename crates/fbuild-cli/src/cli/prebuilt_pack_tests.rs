//! Wiring tests for the hidden artifact builder (#1882): the command packs a
//! real tree, writes the entry the client reads, refuses what cannot be served,
//! and stays out of `--help`.
use super::{PackArgs, run_prebuilt_pack};
use clap::CommandFactory as _;
use serde_json::Value;
use tempfile::TempDir;

const FP: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

fn objects(dir: &TempDir) -> std::path::PathBuf {
    let root = dir.path().join("objects");
    std::fs::create_dir_all(root.join("cores")).unwrap();
    std::fs::write(root.join("cores/main.c.o"), b"compiled core object").unwrap();
    std::fs::write(root.join("cores/hal.c.o"), b"another object").unwrap();
    root
}

fn args(root: &std::path::Path, out: &std::path::Path) -> PackArgs {
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
        arch: "esp32".into(),
        abi: "gcc@14.2.0".into(),
        base_url: "https://bucket.example".into(),
        min_client_version: 1,
    }
}

#[test]
fn the_command_writes_an_archive_and_the_entry_the_client_reads() {
    let dir = TempDir::new().unwrap();
    let root = objects(&dir);
    let out = dir.path().join("out");
    run_prebuilt_pack(args(&root, &out)).unwrap();

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
    let root = objects(&dir);
    let out = dir.path().join("out");
    let mut bad = args(&root, &out);
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
