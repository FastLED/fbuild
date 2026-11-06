//! `[platformio] extra_configs` must be merged, and never silently skipped.
//!
//! PlatformIO merges the listed INI files over `platformio.ini`, so an
//! environment's framework/board/flags -- or an entire `[env:*]` section -- can
//! live in a file other than the base one. Reading only the base file would
//! build firmware from a strict subset of what the project declares
//! (FastLED/fbuild#1929, the same class as #1664/#1730).

use super::PlatformIOConfig;

/// Write `platformio.ini` (plus any sibling files) into a fresh directory.
fn project(ini: &str, extra: &[(&str, &str)]) -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    for (name, body) in extra {
        let path = root.path().join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, body).unwrap();
    }
    std::fs::write(root.path().join("platformio.ini"), ini).unwrap();
    root
}

fn load(ini: &str, extra: &[(&str, &str)]) -> fbuild_core::Result<PlatformIOConfig> {
    let root = project(ini, extra);
    PlatformIOConfig::from_path(&root.path().join("platformio.ini"))
}

/// The failure message, failing loudly when the project was accepted instead.
fn refusal(ini: &str, extra: &[(&str, &str)]) -> String {
    match load(ini, extra) {
        Ok(config) => panic!(
            "expected the project to be refused; it parsed with environments {:?}",
            config.get_environments()
        ),
        Err(error) => error.to_string(),
    }
}

fn option(config: &PlatformIOConfig, env: &str, key: &str) -> String {
    config
        .get_env_config(env)
        .unwrap_or_else(|error| panic!("env {env}: {error}"))
        .get(key)
        .unwrap_or_else(|| panic!("env {env} has no {key}"))
        .clone()
}

const BASE: &str = "[env:s3]\nplatform = espressif32\nboard = esp32-s3-devkitc-1\n\
                    framework = arduino\n";

/// The example from PlatformIO's own `extra_configs` documentation.
#[test]
fn the_documented_example_merges_and_the_later_file_wins() {
    let config = load(
        "[platformio]\nextra_configs =\n  extra_envs.ini\n  extra_debug.ini\n\n\
         [env]\nplatform = espressif32\nframework = espidf\n\n\
         [common]\ndebug_flags = -D RELEASE\nlib_flags = -lc -lm\n\n\
         [env:esp-wrover-kit]\nboard = esp-wrover-kit\n\
         build_flags = ${common.debug_flags}\n",
        &[
            (
                "extra_envs.ini",
                "[env:esp32dev]\nboard = esp32dev\n\
                 build_flags = ${common.lib_flags} ${common.debug_flags}\n\n\
                 [env:lolin32]\nboard = lolin32\nbuild_flags = ${common.debug_flags}\n",
            ),
            (
                "extra_debug.ini",
                "# Override base \"common.debug_flags\"\n\
                 [common]\ndebug_flags = -D DEBUG=1\n\n\
                 [env:lolin32]\nbuild_flags = -Og\n",
            ),
        ],
    )
    .unwrap();

    // Environments declared only in an extra file are real environments.
    assert_eq!(
        config.get_environments(),
        vec!["esp-wrover-kit", "esp32dev", "lolin32"]
    );
    // Base `[env]` data reaches an env that an extra file introduced.
    assert_eq!(option(&config, "esp32dev", "framework"), "espidf");
    assert_eq!(option(&config, "esp32dev", "platform"), "espressif32");
    // Later extra file overwrote earlier `[common]`, and the substitution sees it.
    assert_eq!(
        option(&config, "esp-wrover-kit", "build_flags"),
        "-D DEBUG=1"
    );
    assert_eq!(
        option(&config, "esp32dev", "build_flags"),
        "-lc -lm -D DEBUG=1"
    );
    // ... and the env-level override from the second file wins over the first.
    assert_eq!(option(&config, "lolin32", "build_flags"), "-Og");
}

#[test]
fn a_base_option_survives_when_the_extra_file_does_not_touch_it() {
    let config = load(
        &format!("[platformio]\nextra_configs = extra.ini\n{BASE}"),
        &[("extra.ini", "[env:s3]\nbuild_flags = -DFROM_EXTRA\n")],
    )
    .unwrap();
    assert_eq!(option(&config, "s3", "board"), "esp32-s3-devkitc-1");
    assert_eq!(option(&config, "s3", "build_flags"), "-DFROM_EXTRA");
}

#[test]
fn comma_and_multiline_lists_are_both_accepted() {
    for value in ["a.ini, b.ini", "\n  a.ini\n  b.ini\n"] {
        let config = load(
            &format!("[platformio]\nextra_configs = {value}\n{BASE}"),
            &[
                ("a.ini", "[env:s3]\nboard = from-a\n"),
                ("b.ini", "[env:s3]\nboard = from-b\n"),
            ],
        )
        .unwrap_or_else(|error| panic!("{value:?}: {error}"));
        assert_eq!(option(&config, "s3", "board"), "from-b", "{value:?}");
    }
}

#[test]
fn an_inline_comment_is_not_part_of_the_file_name() {
    let config = load(
        &format!("[platformio]\nextra_configs = extra.ini ; shared debug flags\n{BASE}"),
        &[("extra.ini", "[env:s3]\nboard = from-extra\n")],
    )
    .unwrap();
    assert_eq!(option(&config, "s3", "board"), "from-extra");
}

#[test]
fn relative_paths_resolve_against_the_ini_directory() {
    // The process working directory is the repo, so a cwd-relative lookup
    // could never find `cfg/extra.ini`.
    let config = load(
        &format!("[platformio]\nextra_configs = cfg/extra.ini\n{BASE}"),
        &[("cfg/extra.ini", "[env:s3]\nboard = from-nested\n")],
    )
    .unwrap();
    assert_eq!(option(&config, "s3", "board"), "from-nested");
}

#[test]
fn a_wildcard_expands_in_sorted_order_and_later_files_win() {
    let config = load(
        &format!("[platformio]\nextra_configs = boards/*.ini\n{BASE}"),
        &[
            ("boards/b.ini", "[env:s3]\nbuild_flags = -D B\n"),
            ("boards/a.ini", "[env:s3]\nbuild_flags = -D A\n"),
            ("boards/notes.txt", "[env:s3]\nbuild_flags = -D TXT\n"),
        ],
    )
    .unwrap();
    assert_eq!(option(&config, "s3", "build_flags"), "-D B");
}

#[test]
fn a_missing_file_is_refused_and_named() {
    let message = refusal(
        &format!("[platformio]\nextra_configs = extra_envs.ini\n{BASE}"),
        &[],
    );
    assert!(message.contains("extra_envs.ini"), "{message}");
    assert!(message.contains("does not exist"), "{message}");
}

#[test]
fn a_wildcard_that_matches_nothing_is_refused() {
    let message = refusal(
        &format!("[platformio]\nextra_configs = boards/*.ini\n{BASE}"),
        &[],
    );
    assert!(message.contains("boards/*.ini"), "{message}");
    assert!(message.contains("matched no files"), "{message}");
}

#[test]
fn a_wildcard_that_matches_only_directories_is_refused() {
    let message = refusal(
        &format!("[platformio]\nextra_configs = boards/*\n{BASE}"),
        &[("boards/sub/keep.ini", "[env:s3]\nboard = x\n")],
    );
    // `boards/sub` is a directory, so nothing was actually merged.
    assert!(message.contains("matched no files"), "{message}");
}

#[test]
fn an_extra_file_that_declares_extra_configs_is_refused() {
    // PlatformIO merges the option but never processes it again, so the nested
    // file would silently vanish.
    let message = refusal(
        &format!("[platformio]\nextra_configs = outer.ini\n{BASE}"),
        &[("outer.ini", "[platformio]\nextra_configs = deeper.ini\n")],
    );
    assert!(message.contains("outer.ini"), "{message}");
    assert!(message.contains("deeper.ini"), "{message}");
    assert!(message.contains("recursively"), "{message}");
}

#[test]
fn the_base_file_listing_itself_is_refused() {
    let message = refusal(
        &format!("[platformio]\nextra_configs = platformio.ini\n{BASE}"),
        &[],
    );
    assert!(message.contains("itself"), "{message}");
}

#[test]
fn every_declared_file_is_named_and_inline_comments_are_not() {
    let message = refusal(
        "[platformio]\nextra_configs =\n  extra_envs.ini\n  extra_debug.ini ; shared debug flags\n\
         [env:s3]\nplatform = espressif32\nboard = esp32-s3-devkitc-1\n",
        &[("extra_envs.ini", "")],
    );
    assert!(message.contains("extra_debug.ini"), "{message}");
    assert!(!message.contains("shared debug flags"), "{message}");
}

#[test]
fn an_empty_value_declares_no_file_and_is_accepted() {
    let config = load(
        &format!("[platformio]\ndefault_envs = s3\nextra_configs =   \n{BASE}"),
        &[],
    )
    .unwrap();
    assert_eq!(config.get_default_environment(), Some("s3"));
    assert_eq!(config.get_environments(), vec!["s3"]);
}

#[test]
fn a_comment_only_value_declares_no_file_and_is_accepted() {
    for value in ["; nothing to add", "# nothing to add"] {
        let config = load(
            &format!("[platformio]\nextra_configs = {value}\n{BASE}"),
            &[],
        )
        .unwrap_or_else(|error| panic!("{value:?}: {error}"));
        assert_eq!(config.get_environments(), vec!["s3"]);
    }
}

#[test]
fn extra_configs_outside_the_platformio_section_is_not_a_config_directive() {
    // PlatformIO only honours the option in `[platformio]`; a same-named key in
    // an environment section is not a file include, so refusing it would be a
    // false red.
    let config = load(
        "[env:s3]\nplatform = espressif32\nboard = esp32-s3-devkitc-1\n\
         framework = arduino\nextra_configs = not_a_include.ini\n",
        &[],
    )
    .unwrap();
    assert_eq!(config.get_environments(), vec!["s3"]);
}
