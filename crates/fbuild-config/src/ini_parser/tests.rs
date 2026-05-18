//! Tests for `ini_parser`.

use super::values::{parse_flags, parse_lib_deps, strip_inline_comment};
use super::PlatformIOConfig;
use std::collections::BTreeMap;
use std::io::Write;
use std::path::Path;
use tempfile::NamedTempFile;

fn write_ini(content: &str) -> NamedTempFile {
    let mut f = NamedTempFile::new().unwrap();
    f.write_all(content.as_bytes()).unwrap();
    f.flush().unwrap();
    f
}

fn overrides(pairs: &[(&str, &str)]) -> crate::pio_env::PioEnvOverrides {
    crate::pio_env::PioEnvOverrides::from_map(
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect::<BTreeMap<_, _>>(),
    )
}

#[test]
fn test_init_with_valid_file() {
    let f = write_ini(
        "\
[env:uno]
platform = atmelavr
board = uno
framework = arduino
",
    );
    let config = PlatformIOConfig::from_path(f.path()).unwrap();
    assert_eq!(config.get_environments(), vec!["uno"]);
}

#[test]
fn test_init_with_nonexistent_file() {
    let result = PlatformIOConfig::from_path(Path::new("/nonexistent/platformio.ini"));
    assert!(result.is_err());
}

#[test]
fn test_get_environments_multiple() {
    let f = write_ini(
        "\
[env:uno]
platform = atmelavr
board = uno
framework = arduino

[env:esp32]
platform = espressif32
board = esp32dev
framework = arduino
",
    );
    let config = PlatformIOConfig::from_path(f.path()).unwrap();
    let envs = config.get_environments();
    assert_eq!(envs.len(), 2);
    assert!(envs.contains(&"uno"));
    assert!(envs.contains(&"esp32"));
}

#[test]
fn test_get_environments_empty() {
    let f = write_ini("[platformio]\ndefault_envs = \n");
    let config = PlatformIOConfig::from_path(f.path()).unwrap();
    assert!(config.get_environments().is_empty());
}

#[test]
fn test_get_env_config_valid() {
    let f = write_ini(
        "\
[env:uno]
platform = atmelavr
board = uno
framework = arduino
",
    );
    let config = PlatformIOConfig::from_path(f.path()).unwrap();
    let env = config.get_env_config("uno").unwrap();
    assert_eq!(env.get("platform").unwrap(), "atmelavr");
    assert_eq!(env.get("board").unwrap(), "uno");
}

#[test]
fn test_get_env_config_nonexistent() {
    let f = write_ini("[env:uno]\nplatform = atmelavr\nboard = uno\nframework = arduino\n");
    let config = PlatformIOConfig::from_path(f.path()).unwrap();
    assert!(config.get_env_config("nonexistent").is_err());
}

#[test]
fn test_get_env_config_with_base_env_inheritance() {
    let f = write_ini(
        "\
[env]
framework = arduino

[env:uno]
platform = atmelavr
board = uno
",
    );
    let config = PlatformIOConfig::from_path(f.path()).unwrap();
    let env = config.get_env_config("uno").unwrap();
    assert_eq!(env.get("framework").unwrap(), "arduino");
    assert_eq!(env.get("platform").unwrap(), "atmelavr");
}

#[test]
fn test_get_build_flags_present() {
    let f = write_ini(
        "\
[env:uno]
platform = atmelavr
board = uno
framework = arduino
build_flags = -DFOO -DBAR
",
    );
    let config = PlatformIOConfig::from_path(f.path()).unwrap();
    let flags = config.get_build_flags("uno").unwrap();
    assert_eq!(flags, vec!["-DFOO", "-DBAR"]);
}

#[test]
fn test_get_build_flags_absent() {
    let f = write_ini("[env:uno]\nplatform = atmelavr\nboard = uno\nframework = arduino\n");
    let config = PlatformIOConfig::from_path(f.path()).unwrap();
    let flags = config.get_build_flags("uno").unwrap();
    assert!(flags.is_empty());
}

#[test]
fn test_get_build_flags_prefers_forwarded_override() {
    let f = write_ini(
        "\
[env:uno]
platform = atmelavr
board = uno
framework = arduino
build_flags = -DINI=1
",
    );
    let config = PlatformIOConfig::from_path_with_overrides(
        f.path(),
        overrides(&[("PLATFORMIO_BUILD_FLAGS", "-DOVERRIDE=1 -DFAST=1")]),
    )
    .unwrap();
    assert_eq!(
        config.get_build_flags("uno").unwrap(),
        vec!["-DOVERRIDE=1", "-DFAST=1"]
    );
}

#[test]
fn test_get_build_flags_multiline() {
    let f = write_ini(
        "\
[env:uno]
platform = atmelavr
board = uno
framework = arduino
build_flags =
    -DFOO
    -DBAR
    -DBAZ
",
    );
    let config = PlatformIOConfig::from_path(f.path()).unwrap();
    let flags = config.get_build_flags("uno").unwrap();
    assert_eq!(flags, vec!["-DFOO", "-DBAR", "-DBAZ"]);
}

#[test]
fn test_get_build_flags_d_space_normalization() {
    let f = write_ini(
        "\
[env:uno]
platform = atmelavr
board = uno
framework = arduino
build_flags = -D FOO -D BAR
",
    );
    let config = PlatformIOConfig::from_path(f.path()).unwrap();
    let flags = config.get_build_flags("uno").unwrap();
    assert_eq!(flags, vec!["-DFOO", "-DBAR"]);
}

#[test]
fn test_get_build_src_flags_prefers_forwarded_override() {
    let f = write_ini(
        "\
[env:uno]
platform = atmelavr
board = uno
framework = arduino
build_src_flags = -DINI_SRC
",
    );
    let config = PlatformIOConfig::from_path_with_overrides(
        f.path(),
        overrides(&[("PLATFORMIO_BUILD_SRC_FLAGS", "-DOVERRIDE_SRC=1")]),
    )
    .unwrap();
    assert_eq!(
        config.get_build_src_flags("uno").unwrap(),
        vec!["-DOVERRIDE_SRC=1"]
    );
}

#[test]
fn test_get_extra_scripts_multiline_and_comma_separated() {
    let f = write_ini(
        "\
[env:uno]
platform = atmelavr
board = uno
framework = arduino
extra_scripts =
    pre:scripts/pre.py, scripts/post.py
    post:scripts/after.py
",
    );
    let config = PlatformIOConfig::from_path(f.path()).unwrap();
    let scripts = config.get_extra_scripts("uno").unwrap();
    assert_eq!(
        scripts,
        vec![
            "pre:scripts/pre.py",
            "scripts/post.py",
            "post:scripts/after.py"
        ]
    );
}

#[test]
fn test_get_lib_deps_present() {
    let f = write_ini(
        "\
[env:uno]
platform = atmelavr
board = uno
framework = arduino
lib_deps =
    FastLED
    ArduinoJson
",
    );
    let config = PlatformIOConfig::from_path(f.path()).unwrap();
    let deps = config.get_lib_deps("uno").unwrap();
    assert_eq!(deps, vec!["FastLED", "ArduinoJson"]);
}

#[test]
fn test_get_lib_deps_comma_separated() {
    let f = write_ini(
        "\
[env:uno]
platform = atmelavr
board = uno
framework = arduino
lib_deps = FastLED, ArduinoJson
",
    );
    let config = PlatformIOConfig::from_path(f.path()).unwrap();
    let deps = config.get_lib_deps("uno").unwrap();
    assert_eq!(deps, vec!["FastLED", "ArduinoJson"]);
}

#[test]
fn test_get_lib_deps_absent() {
    let f = write_ini("[env:uno]\nplatform = atmelavr\nboard = uno\nframework = arduino\n");
    let config = PlatformIOConfig::from_path(f.path()).unwrap();
    let deps = config.get_lib_deps("uno").unwrap();
    assert!(deps.is_empty());
}

#[test]
fn test_has_environment() {
    let f = write_ini("[env:uno]\nplatform = atmelavr\nboard = uno\nframework = arduino\n");
    let config = PlatformIOConfig::from_path(f.path()).unwrap();
    assert!(config.has_environment("uno"));
    assert!(!config.has_environment("esp32"));
}

#[test]
fn test_get_default_environment_explicit() {
    let f = write_ini(
        "\
[platformio]
default_envs = esp32

[env:uno]
platform = atmelavr
board = uno
framework = arduino

[env:esp32]
platform = espressif32
board = esp32dev
framework = arduino
",
    );
    let config = PlatformIOConfig::from_path(f.path()).unwrap();
    assert_eq!(config.get_default_environment(), Some("esp32"));
}

#[test]
fn test_get_default_environment_prefers_forwarded_override() {
    let f = write_ini(
        "\
[platformio]
default_envs = esp32

[env:uno]
platform = atmelavr
board = uno
framework = arduino

[env:esp32]
platform = espressif32
board = esp32dev
framework = arduino
",
    );
    let config = PlatformIOConfig::from_path_with_overrides(
        f.path(),
        overrides(&[("PLATFORMIO_DEFAULT_ENVS", "uno")]),
    )
    .unwrap();
    assert_eq!(config.get_default_environment(), Some("uno"));
}

#[test]
fn test_get_default_environment_first_fallback() {
    let f = write_ini(
        "\
[env:alpha]
platform = atmelavr
board = uno
framework = arduino

[env:beta]
platform = espressif32
board = esp32dev
framework = arduino
",
    );
    let config = PlatformIOConfig::from_path(f.path()).unwrap();
    // Should return first alphabetically
    assert_eq!(config.get_default_environment(), Some("alpha"));
}

#[test]
fn test_get_default_environment_none() {
    let f = write_ini("[platformio]\n");
    let config = PlatformIOConfig::from_path(f.path()).unwrap();
    assert_eq!(config.get_default_environment(), None);
}

#[test]
fn test_extends_inheritance() {
    let f = write_ini(
        "\
[env:base]
platform = atmelavr
framework = arduino
build_flags = -DBASE

[env:child]
extends = env:base
board = uno
build_flags = -DCHILD
",
    );
    let config = PlatformIOConfig::from_path(f.path()).unwrap();
    let env = config.get_env_config("child").unwrap();
    assert_eq!(env.get("platform").unwrap(), "atmelavr");
    assert_eq!(env.get("framework").unwrap(), "arduino");
    assert_eq!(env.get("board").unwrap(), "uno");
    // Child overrides parent's build_flags
    assert_eq!(env.get("build_flags").unwrap(), "-DCHILD");
}

#[test]
fn test_get_src_dir_from_ini() {
    let f = write_ini(
        "\
[platformio]
src_dir = custom_src

[env:uno]
platform = atmelavr
board = uno
framework = arduino
",
    );
    let config = PlatformIOConfig::from_path(f.path()).unwrap();
    assert_eq!(
        config.get_src_dir("uno").unwrap(),
        Some("custom_src".to_string())
    );
}

#[test]
fn test_get_src_dir_returns_none_when_not_set() {
    let f = write_ini("[env:uno]\nplatform = atmelavr\nboard = uno\nframework = arduino\n");
    let config = PlatformIOConfig::from_path(f.path()).unwrap();
    assert_eq!(config.get_src_dir("uno").unwrap(), None);
}

#[test]
fn test_get_src_dir_with_inline_comment() {
    let f = write_ini(
        "\
[platformio]
src_dir = custom_src ; this is the source dir

[env:uno]
platform = atmelavr
board = uno
framework = arduino
",
    );
    let config = PlatformIOConfig::from_path(f.path()).unwrap();
    assert_eq!(
        config.get_src_dir("uno").unwrap(),
        Some("custom_src".to_string())
    );
}

#[test]
fn test_get_src_dir_prefers_forwarded_override() {
    let f = write_ini(
        "\
[platformio]
src_dir = ini_src

[env:uno]
platform = atmelavr
board = uno
framework = arduino
",
    );
    let config = PlatformIOConfig::from_path_with_overrides(
        f.path(),
        overrides(&[("PLATFORMIO_SRC_DIR", "override_src")]),
    )
    .unwrap();
    assert_eq!(
        config.get_src_dir("uno").unwrap(),
        Some("override_src".to_string())
    );
}

#[test]
fn test_real_world_config() {
    let f = write_ini(
        "\
[platformio]
default_envs = esp32dev

[env]
framework = arduino

[env:esp32dev]
platform = espressif32
board = esp32dev
build_flags =
    -DFASTLED_ESP32
    -DCORE_DEBUG_LEVEL=0
lib_deps =
    FastLED
    ArduinoJson

[env:uno]
platform = atmelavr
board = uno
build_flags = -DFASTLED_AVR
",
    );
    let config = PlatformIOConfig::from_path(f.path()).unwrap();

    // Default env
    assert_eq!(config.get_default_environment(), Some("esp32dev"));

    // ESP32 config
    let esp = config.get_env_config("esp32dev").unwrap();
    assert_eq!(esp.get("platform").unwrap(), "espressif32");
    assert_eq!(esp.get("framework").unwrap(), "arduino"); // inherited from [env]

    let esp_flags = config.get_build_flags("esp32dev").unwrap();
    assert_eq!(esp_flags, vec!["-DFASTLED_ESP32", "-DCORE_DEBUG_LEVEL=0"]);

    let esp_deps = config.get_lib_deps("esp32dev").unwrap();
    assert_eq!(esp_deps, vec!["FastLED", "ArduinoJson"]);

    // Uno config
    let uno = config.get_env_config("uno").unwrap();
    assert_eq!(uno.get("platform").unwrap(), "atmelavr");
    assert_eq!(uno.get("framework").unwrap(), "arduino"); // inherited from [env]

    let uno_flags = config.get_build_flags("uno").unwrap();
    assert_eq!(uno_flags, vec!["-DFASTLED_AVR"]);
}

#[test]
fn test_strip_inline_comment() {
    assert_eq!(strip_inline_comment("value ; comment"), "value");
    assert_eq!(strip_inline_comment("value # comment"), "value");
    assert_eq!(strip_inline_comment("value"), "value");
    assert_eq!(strip_inline_comment("#include <foo>"), "#include <foo>");
}

#[test]
fn test_parse_flags() {
    assert_eq!(parse_flags("-DFOO -DBAR"), vec!["-DFOO", "-DBAR"]);
    assert_eq!(parse_flags("-D FOO -D BAR"), vec!["-DFOO", "-DBAR"]);
    assert_eq!(
        parse_flags("-DFOO\n-DBAR\n-DBAZ"),
        vec!["-DFOO", "-DBAR", "-DBAZ"]
    );
}

#[test]
fn test_parse_lib_deps() {
    assert_eq!(
        parse_lib_deps("FastLED, ArduinoJson"),
        vec!["FastLED", "ArduinoJson"]
    );
    assert_eq!(
        parse_lib_deps("FastLED\nArduinoJson"),
        vec!["FastLED", "ArduinoJson"]
    );
}

#[test]
fn test_get_lib_ignore_present() {
    let f = write_ini(
        "\
[env:esp32dev]
platform = espressif32
board = esp32dev
framework = arduino
lib_ignore =
    WiFi
    FS
",
    );
    let config = PlatformIOConfig::from_path(f.path()).unwrap();
    let ignore = config.get_lib_ignore("esp32dev").unwrap();
    assert_eq!(ignore, vec!["WiFi", "FS"]);
}

#[test]
fn test_get_lib_ignore_absent() {
    let f = write_ini("[env:uno]\nplatform = atmelavr\nboard = uno\nframework = arduino\n");
    let config = PlatformIOConfig::from_path(f.path()).unwrap();
    let ignore = config.get_lib_ignore("uno").unwrap();
    assert!(ignore.is_empty());
}

#[test]
fn test_variable_substitution_follows_extends_chain() {
    // Mirrors NightDriverStrip pattern: [base] defines build_flags,
    // [dev_esp32] extends base (no build_flags), [env:demo] references
    // ${dev_esp32.build_flags} — must resolve through the extends chain.
    let f = write_ini(
        "\
[base]
build_flags = -std=gnu++2a
              -Ofast

[dev_esp32]
extends = base
board = esp32dev

[env:demo]
extends = dev_esp32
platform = espressif32
framework = arduino
build_flags = -DDEMO=1
              ${dev_esp32.build_flags}
",
    );
    let config = PlatformIOConfig::from_path(f.path()).unwrap();
    let flags = config.get_build_flags("demo").unwrap();
    assert!(
        flags.contains(&"-DDEMO=1".to_string()),
        "should contain -DDEMO=1, got: {:?}",
        flags
    );
    assert!(
        flags.contains(&"-std=gnu++2a".to_string()),
        "should resolve ${{dev_esp32.build_flags}} through extends chain to [base], got: {:?}",
        flags
    );
    assert!(
        flags.contains(&"-Ofast".to_string()),
        "should contain -Ofast from [base], got: {:?}",
        flags
    );
    // Must NOT contain the literal unresolved variable
    let raw = config.get_env_config("demo").unwrap();
    let raw_flags = raw.get("build_flags").unwrap();
    assert!(
        !raw_flags.contains("${"),
        "build_flags should not contain unresolved variables, got: {}",
        raw_flags
    );
}

#[test]
fn test_non_env_extends_inherits_lib_deps() {
    // [env:demo] extends [dev_esp32] extends [base].
    // lib_deps is only on [base] — must propagate through.
    let f = write_ini(
        "\
[base]
lib_deps = fastled/FastLED@^3.7.8
           bblanchon/ArduinoJson@^7.0.0

[dev_esp32]
extends = base
board = esp32dev

[env:demo]
extends = dev_esp32
platform = espressif32
framework = arduino
",
    );
    let config = PlatformIOConfig::from_path(f.path()).unwrap();
    let deps = config.get_lib_deps("demo").unwrap();
    assert!(
        deps.iter().any(|d| d.contains("FastLED")),
        "should inherit lib_deps from [base] via [dev_esp32], got: {:?}",
        deps
    );
    assert!(
        deps.iter().any(|d| d.contains("ArduinoJson")),
        "should inherit ArduinoJson from [base], got: {:?}",
        deps
    );
}

#[test]
fn test_get_embed_files() {
    let f = write_ini(
        "\
[env:demo]
platform = espressif32
board = esp32dev
framework = arduino
board_build.embed_files = site/dist/index.html.gz
                          site/dist/favicon.ico.gz
board_build.embed_txtfiles = config/timezones.json
",
    );
    let config = PlatformIOConfig::from_path(f.path()).unwrap();
    let embed = config.get_embed_files("demo").unwrap();
    assert_eq!(embed.len(), 2);
    assert!(embed.contains(&"site/dist/index.html.gz".to_string()));
    assert!(embed.contains(&"site/dist/favicon.ico.gz".to_string()));

    let txt = config.get_embed_txtfiles("demo").unwrap();
    assert_eq!(txt.len(), 1);
    assert_eq!(txt[0], "config/timezones.json");
}

#[test]
fn test_get_embed_files_empty() {
    let f = write_ini("[env:uno]\nplatform = atmelavr\nboard = uno\nframework = arduino\n");
    let config = PlatformIOConfig::from_path(f.path()).unwrap();
    assert!(config.get_embed_files("uno").unwrap().is_empty());
    assert!(config.get_embed_txtfiles("uno").unwrap().is_empty());
}

#[test]
fn test_get_source_filter_prefers_build_src_filter() {
    let f = write_ini(
        "\
[env:demo]
platform = espressif32
board = esp32dev
framework = arduino
src_filter = +<legacy.cpp>
build_src_filter =
    +<*>
    -<generated/>
",
    );
    let config = PlatformIOConfig::from_path(f.path()).unwrap();
    assert_eq!(
        config.get_source_filter("demo").unwrap(),
        Some("+<*>\n-<generated/>".to_string())
    );
}

#[test]
fn test_get_source_filter_falls_back_to_src_filter() {
    let f = write_ini(
        "\
[env:demo]
platform = ststm32
board = bluepill_f103c8
framework = stm32cube
src_filter =
    +<main.cpp>
    -<legacy/>
",
    );
    let config = PlatformIOConfig::from_path(f.path()).unwrap();
    assert_eq!(
        config.get_source_filter("demo").unwrap(),
        Some("+<main.cpp>\n-<legacy/>".to_string())
    );
}

#[test]
fn test_get_build_unflags_prefers_env_override() {
    let f = write_ini(
        "\
[env:demo]
platform = espressif32
board = esp32dev
framework = arduino
build_unflags = -std=gnu++11
",
    );
    let overrides = crate::pio_env::PioEnvOverrides::from_map(
        [(
            "PLATFORMIO_BUILD_UNFLAGS".to_string(),
            "-std=gnu++17 -DDEBUG".to_string(),
        )]
        .into_iter()
        .collect(),
    );
    let config = PlatformIOConfig::from_path_with_overrides(f.path(), overrides).unwrap();
    assert_eq!(
        config.get_build_unflags("demo").unwrap(),
        vec!["-std=gnu++17", "-DDEBUG"]
    );
}

#[test]
fn test_get_build_unflags_from_ini() {
    let f = write_ini(
        "\
[env:demo]
platform = ststm32
board = bluepill_f103c8
framework = stm32cube
build_unflags =
    -Os
    -DDEBUG
",
    );
    let config = PlatformIOConfig::from_path(f.path()).unwrap();
    assert_eq!(
        config.get_build_unflags("demo").unwrap(),
        vec!["-Os", "-DDEBUG"]
    );
}

#[test]
fn test_get_build_type_defaults_to_release() {
    let f = write_ini(
        "\
[env:demo]
platform = atmelavr
board = uno
framework = arduino
",
    );
    let config = PlatformIOConfig::from_path(f.path()).unwrap();
    assert_eq!(config.get_build_type("demo").unwrap(), "release");
}

#[test]
fn test_get_build_type_reads_debug() {
    let f = write_ini(
        "\
[env:demo]
platform = espressif32
board = esp32dev
framework = arduino
build_type = debug
",
    );
    let config = PlatformIOConfig::from_path(f.path()).unwrap();
    assert_eq!(config.get_build_type("demo").unwrap(), "debug");
}

#[test]
fn test_get_debug_build_flags_uses_platformio_defaults() {
    let f = write_ini(
        "\
[env:demo]
platform = espressif32
board = esp32dev
framework = arduino
build_type = debug
",
    );
    let config = PlatformIOConfig::from_path(f.path()).unwrap();
    assert_eq!(
        config.get_debug_build_flags("demo").unwrap(),
        vec!["-Og", "-g2", "-ggdb2"]
    );
}

#[test]
fn test_get_debug_build_flags_reads_ini_override() {
    let f = write_ini(
        "\
[env:demo]
platform = espressif32
board = esp32dev
framework = arduino
build_type = debug
debug_build_flags = -Og -g3
",
    );
    let config = PlatformIOConfig::from_path(f.path()).unwrap();
    assert_eq!(
        config.get_debug_build_flags("demo").unwrap(),
        vec!["-Og", "-g3"]
    );
}
