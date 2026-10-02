//! Pure rules that adapt the ESP8266 recipe to a board's `boards.txt` menu
//! selections. Each rule is a side-effect-free `config -> config` function;
//! the orchestrator applies [`for_board_props`] once, in one line.

use std::collections::HashMap;

use super::mcu_config::Esp8266McuConfig;
use crate::esp32::mcu_config::DefineEntry;

/// The recipe adapted to the board's `boards.txt` menu selections: the SDK
/// define, `-D` overrides from the flag properties, and the lwIP and libstdc++
/// variants.
pub(super) fn for_board_props(
    config: Esp8266McuConfig,
    board_props: &Option<HashMap<String, String>>,
) -> Esp8266McuConfig {
    let Some(props) = board_props.as_ref() else {
        return config;
    };
    let config = match props.get("sdk") {
        Some(sdk_name) => with_sdk_define(config, sdk_name),
        None => config,
    };
    let config = board_define_flags(props)
        .into_iter()
        .fold(config, |config, (name, value)| {
            with_define(config, name, value)
        });
    let config = match props.get("lwip_lib") {
        Some(lib) => with_first_lib_replaced(config, |l| l.starts_with("-llwip"), lib),
        None => config,
    };
    match props.get("stdcpp_lib") {
        Some(lib) => {
            with_first_lib_replaced(config, |l| l == "-lstdc++" || l == "-lstdc++-exc", lib)
        }
        None => config,
    }
}

/// Replace the recipe's `NONOSDK*` key/value define with the board's SDK.
fn with_sdk_define(mut config: Esp8266McuConfig, sdk_name: &str) -> Esp8266McuConfig {
    config.defines.retain(
        |entry| !matches!(entry, DefineEntry::KeyValue(name, _) if name.starts_with("NONOSDK")),
    );
    config
        .defines
        .push(DefineEntry::KeyValue(sdk_name.to_string(), "1".to_string()));
    config
}

/// The `-D` defines in the board's flag properties, in application order.
fn board_define_flags(props: &HashMap<String, String>) -> Vec<(String, String)> {
    ["flash_flags", "lwip_flags", "mmuflags", "vtable_flags"]
        .into_iter()
        .filter_map(|key| props.get(key))
        .flat_map(|flags| fbuild_core::shell_split::split(flags))
        .filter_map(|token| {
            let def = token.strip_prefix("-D")?;
            Some(
                def.split_once('=')
                    .map(|(name, value)| (name.to_string(), value.to_string()))
                    .unwrap_or_else(|| (def.to_string(), "1".to_string())),
            )
        })
        .collect()
}

/// Set `name` to `value`, replacing any existing define of that name.
fn with_define(mut config: Esp8266McuConfig, name: String, value: String) -> Esp8266McuConfig {
    config.defines.retain(|entry| match entry {
        DefineEntry::Simple(existing) | DefineEntry::KeyValue(existing, _) => existing != &name,
    });
    config.defines.push(DefineEntry::KeyValue(name, value));
    config
}

/// Replace the first linker lib that `matches` with `lib`.
fn with_first_lib_replaced(
    mut config: Esp8266McuConfig,
    matches: impl Fn(&str) -> bool,
    lib: &str,
) -> Esp8266McuConfig {
    if let Some(slot) = config.linker_libs.iter_mut().find(|l| matches(l)) {
        *slot = lib.to_string();
    }
    config
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::esp8266::mcu_config::get_esp8266_config;

    fn define_names(config: &Esp8266McuConfig) -> Vec<(String, Option<String>)> {
        config
            .defines
            .iter()
            .map(|entry| match entry {
                DefineEntry::Simple(name) => (name.clone(), None),
                DefineEntry::KeyValue(name, value) => (name.clone(), Some(value.clone())),
            })
            .collect()
    }

    #[test]
    fn board_props_none_keeps_recipe() {
        let base = get_esp8266_config().unwrap();
        let config = for_board_props(base.clone(), &None);
        assert_eq!(define_names(&config), define_names(&base));
        assert_eq!(config.linker_libs, base.linker_libs);
    }

    #[test]
    fn board_props_rewrite_sdk_defines_and_libs() {
        let base = get_esp8266_config().unwrap();
        let props = HashMap::from([
            ("sdk".to_string(), "NONOSDK3V0".to_string()),
            (
                "mmuflags".to_string(),
                "-DMMU_IRAM_SIZE=0xC000 -DMMU_ICACHE_SIZE=0x4000 -DFOO".to_string(),
            ),
            (
                "vtable_flags".to_string(),
                "-DMMU_IRAM_SIZE=0x8000".to_string(),
            ),
            ("lwip_lib".to_string(), "-llwip2-1460-feat".to_string()),
            ("stdcpp_lib".to_string(), "-lstdc++-exc".to_string()),
        ]);
        let config = for_board_props(base.clone(), &Some(props));
        let defines = define_names(&config);

        assert!(!defines.iter().any(|(name, _)| name == "NONOSDK22x_190703"));
        assert!(defines.contains(&("NONOSDK3V0".into(), Some("1".into()))));
        // Later properties override earlier ones; each name appears once.
        let iram: Vec<_> = defines
            .iter()
            .filter(|(n, _)| n == "MMU_IRAM_SIZE")
            .collect();
        assert_eq!(
            iram,
            [&("MMU_IRAM_SIZE".to_string(), Some("0x8000".to_string()))]
        );
        assert!(defines.contains(&("FOO".into(), Some("1".into()))));

        assert!(
            config
                .linker_libs
                .contains(&"-llwip2-1460-feat".to_string())
        );
        assert!(!config.linker_libs.contains(&"-llwip2-536-feat".to_string()));
        assert!(config.linker_libs.contains(&"-lstdc++-exc".to_string()));
        assert!(!config.linker_libs.contains(&"-lstdc++".to_string()));
        assert_eq!(config.linker_libs.len(), base.linker_libs.len());
    }
}
