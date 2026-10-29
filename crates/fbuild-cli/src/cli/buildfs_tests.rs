//! Unit + acceptance tests for `fbuild buildfs` (FastLED/fbuild#1358).

use super::*;

const DEFAULT_CSV: &str = "\
# ESP32 default partition table
nvs,      data, nvs,     0x9000,  0x5000,
otadata,  data, ota,     0xe000,  0x2000,
app0,     app,  ota_0,   0x10000, 0x140000,
app1,     app,  ota_1,   0x150000,0x140000,
spiffs,   data, spiffs,  0x290000,0x160000,
coredump, data, coredump,0x3F0000,0x10000,
";

#[test]
fn parses_partition_table_and_sizes() {
    let table = parse_partition_table(DEFAULT_CSV).expect("table parses");
    assert_eq!(table.len(), 6);
    let fs = select_fs_partition(&table, None).expect("fs partition found");
    assert_eq!(fs.label, "spiffs");
    assert_eq!(fs.offset, 0x29_0000);
    assert_eq!(fs.size, 0x16_0000);
    assert_eq!(fs_kind_for_subtype(&fs.subtype), Some(FsKind::Spiffs));
}

#[test]
fn size_syntax_accepts_decimal_hex_and_suffixes() {
    assert_eq!(parse_partition_size("0x160000"), Some(0x16_0000));
    assert_eq!(parse_partition_size("1441792"), Some(1_441_792));
    assert_eq!(parse_partition_size("32K"), Some(32 * 1024));
    assert_eq!(parse_partition_size("2M"), Some(2 * 1024 * 1024));
    assert_eq!(parse_partition_size("nope"), None);
}

#[test]
fn selects_littlefs_partition_and_numeric_subtypes() {
    let csv = "fs, data, littlefs, 0x200000, 0x100000,\nnvs, data, nvs, 0x9000, 0x5000,\n";
    let table = parse_partition_table(csv).expect("table parses");
    let fs = select_fs_partition(&table, None).expect("fs partition found");
    assert_eq!(fs.label, "fs");
    assert_eq!(fs_kind_for_subtype(&fs.subtype), Some(FsKind::LittleFs));

    let numeric = parse_partition_table("s, data, 0x83, 0x100000, 0x100000,\n").unwrap();
    assert_eq!(
        fs_kind_for_subtype(&numeric[0].subtype),
        Some(FsKind::LittleFs)
    );
}

#[test]
fn explicit_label_wins_and_missing_label_lists_choices() {
    let table = parse_partition_table(DEFAULT_CSV).unwrap();
    let by_label = select_fs_partition(&table, Some("coredump")).expect("label exists");
    assert_eq!(by_label.subtype, "coredump");

    let err = select_fs_partition(&table, Some("nope")).expect_err("missing label");
    let message = format!("{err}");
    assert!(message.contains("nope"), "{message}");
    assert!(message.contains("spiffs"), "{message}");
}

#[test]
fn table_without_filesystem_partition_is_an_actionable_error() {
    let csv = "nvs, data, nvs, 0x9000, 0x5000,\napp0, app, factory, 0x10000, 0x100000,\n";
    let table = parse_partition_table(csv).unwrap();
    let err = select_fs_partition(&table, None).expect_err("no fs partition");
    let message = format!("{err}");
    assert!(message.contains("no data partition"), "{message}");
    assert!(message.contains("--fs-partition"), "{message}");
}

#[test]
fn estimate_aligns_to_pages_and_reserves_metadata() {
    let single = estimate_image_bytes(&[StagedFile {
        relative: "a.txt".to_string(),
        bytes: 1,
    }]);
    assert_eq!(single, PAGE_SIZE + PAGE_SIZE + METADATA_BLOCKS * BLOCK_SIZE);

    let nested = estimate_image_bytes(&[StagedFile {
        relative: "dir/sub/a.bin".to_string(),
        bytes: 7000,
    }]);
    assert_eq!(
        nested,
        align_up(7000, PAGE_SIZE) + 3 * PAGE_SIZE + METADATA_BLOCKS * BLOCK_SIZE
    );
}

#[test]
fn capacity_reports_the_shortfall() {
    let partition = Partition {
        label: "spiffs".to_string(),
        kind: "data".to_string(),
        subtype: "spiffs".to_string(),
        offset: 0,
        size: 100_000,
    };
    assert!(check_capacity(100_000, &partition).is_ok());
    let err = check_capacity(120_000, &partition).expect_err("over capacity");
    let message = format!("{err}");
    assert!(message.contains("over by 20000 bytes"), "{message}");
}

#[test]
fn tool_argv_uses_the_partition_size_as_the_image_bound() {
    let args = fs_tool_args(
        Path::new("/stage"),
        0x16_0000,
        Path::new("/out/littlefs.bin"),
    );
    assert_eq!(
        args,
        vec![
            "-c",
            "/stage",
            "-b",
            "4096",
            "-p",
            "256",
            "-s",
            "1441792",
            "/out/littlefs.bin",
        ]
    );
}

#[test]
fn pointer_suffixes_are_projected_away() {
    assert_eq!(strip_pointer_suffix("a/b/foo.png.lnk"), "a/b/foo.png");
    assert_eq!(strip_pointer_suffix("a/b/foo.png.fetch"), "a/b/foo.png");
    assert_eq!(strip_pointer_suffix("a/b/foo.png"), "a/b/foo.png");
}

#[test]
fn filesystem_names_are_parsed_and_mapped_to_tools() {
    assert_eq!(FsKind::parse("LittleFS"), Some(FsKind::LittleFs));
    assert_eq!(FsKind::parse("spiffs"), Some(FsKind::Spiffs));
    assert_eq!(FsKind::parse("ffat"), None);
    assert_eq!(FsKind::LittleFs.image_name(), "littlefs.bin");
    assert_eq!(FsKind::Spiffs.tool_package(), "tool-mkspiffs");
}

/// Build a throwaway project with a `data/` tree and an explicit partition CSV.
fn scratch_project(name: &str) -> (tempfile::TempDir, NormalizedPath, NormalizedPath) {
    let root =
        tempfile::tempdir_in(fbuild_paths::dev_or_prod_temp_root()).expect("scratch tempdir");
    let project = root.path().join(name);
    std::fs::create_dir_all(project.join("data/nested")).unwrap();
    std::fs::write(project.join("data/hello.txt"), b"hello fs").unwrap();
    std::fs::write(project.join("data/nested/blob.bin"), b"payload").unwrap();
    // FastLED's on-device asset link: plain text, not an fbuild pointer.
    std::fs::write(project.join("data/asset.lnk"), b"/assets/font.bin\n").unwrap();
    let csv = root.path().join("partitions.csv");
    std::fs::write(
        &csv,
        "nvs, data, nvs, 0x9000, 0x5000,\nfs, data, littlefs, 0x200000, 0x100000,\n",
    )
    .unwrap();
    (root, NormalizedPath::new(project), NormalizedPath::new(csv))
}

#[test]
fn staging_ships_plain_asset_links_and_skips_nothing_else() {
    let (_root, project, _csv) = scratch_project("stage");
    let staging = NormalizedPath::new(project.as_path().join("stage_out"));
    stage_data(
        &NormalizedPath::new(project.as_path().join("data")),
        &staging,
    )
    .expect("staging succeeds");
    let files = measure_staging(&staging).expect("measure");
    let names: Vec<&str> = files.iter().map(|f| f.relative.as_str()).collect();
    assert_eq!(names, vec!["asset.lnk", "hello.txt", "nested/blob.bin"]);
}

#[tokio::test]
async fn check_only_reports_and_does_not_build_an_image() {
    let (_root, project, csv) = scratch_project("checkonly");
    let args = BuildfsArgs {
        project_dir: None,
        environment: None,
        data_dir: None,
        partitions: Some(csv.display().to_string()),
        fs_partition: None,
        filesystem: Some("littlefs".to_string()),
        tool: None,
        output: None,
        check_only: true,
        json: true,
    };
    run(args, project.as_path())
        .await
        .expect("check-only succeeds");
}

#[tokio::test]
async fn oversized_data_fails_before_the_image_tool_runs() {
    let (_root, project, csv) = scratch_project("oversize");
    std::fs::write(
        project.as_path().join("data/big.bin"),
        vec![0u8; 2 * 1024 * 1024],
    )
    .unwrap();
    let args = BuildfsArgs {
        project_dir: None,
        environment: None,
        data_dir: None,
        partitions: Some(csv.display().to_string()),
        fs_partition: None,
        filesystem: Some("littlefs".to_string()),
        // Deliberately bogus: the size check must fail before tool resolution
        // or any spawn would notice.
        tool: Some("/nonexistent/mklittlefs".to_string()),
        output: None,
        check_only: false,
        json: false,
    };
    let err = run(args, project.as_path())
        .await
        .expect_err("oversize data fails");
    let message = format!("{err}");
    assert!(message.contains("does not fit"), "{message}");
    assert!(message.contains("over by"), "{message}");
}

#[tokio::test]
async fn missing_data_dir_names_the_directory() {
    let (_root, project, csv) = scratch_project("nodata");
    std::fs::remove_dir_all(project.as_path().join("data")).unwrap();
    let args = BuildfsArgs {
        project_dir: None,
        environment: None,
        data_dir: None,
        partitions: Some(csv.display().to_string()),
        fs_partition: None,
        filesystem: None,
        tool: None,
        output: None,
        check_only: true,
        json: false,
    };
    let err = run(args, project.as_path())
        .await
        .expect_err("missing data dir");
    assert!(format!("{err}").contains("does not exist"), "{err}");
}

#[test]
fn plain_text_lnk_is_device_data_not_a_pointer() {
    let root = tempfile::tempdir_in(fbuild_paths::dev_or_prod_temp_root()).unwrap();
    let asset = root.path().join("font.lnk");
    std::fs::write(&asset, b"# runtime asset\n/assets/font.bin\n").unwrap();
    assert!(
        classify_pointer(&asset)
            .expect("plain text is data")
            .is_none()
    );
}

#[test]
fn json_lnk_is_recognised_as_an_fbuild_pointer() {
    let root = tempfile::tempdir_in(fbuild_paths::dev_or_prod_temp_root()).unwrap();
    let pointer = root.path().join("logo.png.lnk");
    let sha = "a".repeat(64);
    std::fs::write(
        &pointer,
        format!("{{\"v\":1,\"url\":\"https://example.com/logo.png\",\"sha256\":\"{sha}\"}}"),
    )
    .unwrap();
    let lnk = classify_pointer(&pointer)
        .expect("valid pointer parses")
        .expect("is a pointer");
    assert_eq!(lnk.url, "https://example.com/logo.png");
}

#[test]
fn broken_pointer_fails_instead_of_shipping() {
    let root = tempfile::tempdir_in(fbuild_paths::dev_or_prod_temp_root()).unwrap();
    // No `v` field: this parses as JSON but is not a valid manifest.
    let pointer = root.path().join("blob.bin.fetch");
    std::fs::write(
        &pointer,
        b"{\"url\":\"https://example.com/x\",\"sha256\":\"00\"}",
    )
    .unwrap();
    let err = classify_pointer(&pointer).expect_err("broken pointer is an error");
    let message = format!("{err}");
    assert!(message.contains("never ship"), "{message}");
    assert!(message.contains("blob.bin.fetch"), "{message}");
}

#[tokio::test]
async fn broken_pointer_aborts_the_build() {
    let (_root, project, csv) = scratch_project("brokenptr");
    std::fs::write(
        project.as_path().join("data/broken.bin.fetch"),
        b"{\"url\":\"https://example.com/x\",\"sha256\":\"00\"}",
    )
    .unwrap();
    let args = BuildfsArgs {
        project_dir: None,
        environment: None,
        data_dir: None,
        partitions: Some(csv.display().to_string()),
        fs_partition: None,
        filesystem: Some("littlefs".to_string()),
        tool: None,
        output: None,
        check_only: true,
        json: false,
    };
    let err = run(args, project.as_path())
        .await
        .expect_err("broken pointer aborts");
    assert!(format!("{err}").contains("does not parse"), "{err}");
}
