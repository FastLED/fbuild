//! `[platformio] extra_configs` must be refused, not silently ignored.
//!
//! PlatformIO merges the listed INI files over `platformio.ini`, so an
//! environment's framework/board/flags can live in a file fbuild never opens.
//! Building anyway would claim a configuration the project did not ask for
//! (FastLED/fbuild#1664, the same class as a bare `framework = espidf`).

use super::PlatformIOConfig;

/// Write `platformio.ini` (plus any sibling files) into a fresh directory.
fn project(ini: &str, extra: &[(&str, &str)]) -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    for (name, body) in extra {
        std::fs::write(root.path().join(name), body).unwrap();
    }
    std::fs::write(root.path().join("platformio.ini"), ini).unwrap();
    root
}

fn load(ini: &str, extra: &[(&str, &str)]) -> fbuild_core::Result<PlatformIOConfig> {
    let root = project(ini, extra);
    PlatformIOConfig::from_path(&root.path().join("platformio.ini"))
}

/// The refusal message, failing loudly when the project was accepted instead.
fn refusal(ini: &str, extra: &[(&str, &str)]) -> String {
    match load(ini, extra) {
        Ok(config) => panic!(
            "expected the project to be refused; it parsed with environments {:?}",
            config.get_environments()
        ),
        Err(error) => error.to_string(),
    }
}

const BASE: &str = "[env:s3]\nplatform = espressif32\nboard = esp32-s3-devkitc-1\n\
                    framework = arduino\n";

#[test]
fn extra_configs_is_rejected_before_the_environment_is_read() {
    // The extra file is what makes this dangerous: it would replace the
    // framework with a bare `espidf` that fbuild cannot build.
    let message = refusal(
        &format!("[platformio]\nextra_configs = extra_envs.ini\n{BASE}"),
        &[("extra_envs.ini", "[env]\nframework = espidf\n")],
    );
    assert!(message.contains("extra_configs"), "{message}");
    assert!(message.contains("extra_envs.ini"), "{message}");
    assert!(
        message.contains("platformio.ini"),
        "the failure must name the file fbuild does read: {message}"
    );
}

#[test]
fn every_declared_file_is_named_and_inline_comments_are_not() {
    let message = refusal(
        "[platformio]\nextra_configs =\n  extra_envs.ini\n  extra_debug.ini ; shared debug flags\n\
         [env:s3]\nplatform = espressif32\nboard = esp32-s3-devkitc-1\n",
        &[],
    );
    assert!(message.contains("extra_envs.ini"), "{message}");
    assert!(message.contains("extra_debug.ini"), "{message}");
    assert!(!message.contains("shared debug flags"), "{message}");
}

#[test]
fn a_mixed_platformio_section_is_still_rejected() {
    let message = refusal(
        "[platformio]\ndefault_envs = s3\nextra_configs = boards/*.ini\n{BASE}",
        &[],
    );
    assert!(message.contains("boards/*.ini"), "{message}");
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
    // an environment section is not a file include, so refusing it here would
    // be a false red.
    let config = load(
        "[env:s3]\nplatform = espressif32\nboard = esp32-s3-devkitc-1\n\
         framework = arduino\nextra_configs = not_a_include.ini\n",
        &[],
    )
    .unwrap();
    assert_eq!(config.get_environments(), vec!["s3"]);
}
