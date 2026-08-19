use std::collections::HashMap;
use std::path::Path;

fn parse_human_size_bytes(raw: &str) -> Option<u64> {
    let value = raw.trim();
    if value.is_empty() {
        return None;
    }
    let number_end = value
        .find(|character: char| !character.is_ascii_digit() && character != '.')
        .unwrap_or(value.len());
    let number = value[..number_end].parse::<f64>().ok()?;
    if !number.is_finite() || number < 0.0 {
        return None;
    }
    let suffix = value[number_end..].trim().to_ascii_lowercase();
    let multiplier = match suffix.as_str() {
        "" | "b" => 1_u64,
        "k" | "kb" | "kib" => 1024,
        "m" | "mb" | "mib" => 1024 * 1024,
        "g" | "gb" | "gib" => 1024 * 1024 * 1024,
        _ => return None,
    };
    Some((number * multiplier as f64).round() as u64)
}

fn flash_menu_option_for_filesystem_size(
    content: &str,
    board_prefix: &str,
    filesystem_size: &str,
    flash_size: Option<&str>,
    default_flash_option: Option<&str>,
) -> Option<String> {
    #[derive(Default)]
    struct FlashGeometry {
        option: String,
        fs_start: Option<u64>,
        fs_end: Option<u64>,
        flash_total: Option<u64>,
    }

    let desired_size = parse_human_size_bytes(filesystem_size)?;
    let flash_prefix = format!("{board_prefix}menu.flash.");
    let mut geometries = Vec::<FlashGeometry>::new();

    for line in content.lines() {
        let trimmed = line.trim();
        let Some((key, value)) = trimmed.split_once('=') else {
            continue;
        };
        let Some(rest) = key.strip_prefix(&flash_prefix) else {
            continue;
        };
        let Some((option, property)) = rest.split_once('.') else {
            continue;
        };
        let geometry = match geometries.iter().position(|entry| entry.option == option) {
            Some(index) => &mut geometries[index],
            None => {
                geometries.push(FlashGeometry {
                    option: option.to_string(),
                    ..FlashGeometry::default()
                });
                geometries.last_mut()?
            }
        };
        match property {
            "build.fs_start" => geometry.fs_start = value.trim().parse().ok(),
            "build.fs_end" => geometry.fs_end = value.trim().parse().ok(),
            "build.flash_total" => geometry.flash_total = value.trim().parse().ok(),
            _ => {}
        }
    }

    let desired_flash_total = flash_size.and_then(parse_human_size_bytes).or_else(|| {
        let default_option = default_flash_option?;
        geometries
            .iter()
            .find(|geometry| geometry.option == default_option)
            .and_then(|geometry| geometry.flash_total)
    });

    geometries.into_iter().find_map(|geometry| {
        let filesystem_matches = geometry.fs_end?.checked_sub(geometry.fs_start?)? == desired_size;
        let flash_matches = match desired_flash_total {
            Some(desired_total) => geometry.flash_total == Some(desired_total),
            None => true,
        };
        (filesystem_matches && flash_matches).then_some(geometry.option)
    })
}

fn insert_prop(props: &mut HashMap<String, String>, key: &str, value: &str) {
    let trimmed_key = key.trim();
    let trimmed_value = value.trim();
    let normalized = trimmed_key
        .strip_prefix("build.")
        .or_else(|| trimmed_key.strip_prefix("upload."))
        .unwrap_or(trimmed_key);
    props.insert(normalized.to_string(), trimmed_value.to_string());
    if normalized != trimmed_key {
        props.insert(trimmed_key.to_string(), trimmed_value.to_string());
    }
}

pub fn load_board_props_with_default_menus(
    boards_txt: &Path,
    board_id: &str,
) -> Option<HashMap<String, String>> {
    load_board_props_with_menu_overrides(boards_txt, board_id, &HashMap::new())
}

/// Load Arduino board properties after applying the board's default menu
/// choices and any PlatformIO `board_build.<menu>` overrides.
pub fn load_board_props_with_menu_overrides(
    boards_txt: &Path,
    board_id: &str,
    menu_overrides: &HashMap<String, String>,
) -> Option<HashMap<String, String>> {
    let content = std::fs::read_to_string(boards_txt).ok()?;
    let prefix = format!("{board_id}.");
    let mut props = HashMap::new();
    let mut menu_defaults = HashMap::new();

    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let Some((key, value)) = trimmed.split_once('=') else {
            continue;
        };
        let Some(rest) = key.strip_prefix(&prefix) else {
            continue;
        };

        if let Some(menu_rest) = rest.strip_prefix("menu.") {
            let parts = menu_rest.split('.').collect::<Vec<_>>();
            if parts.len() >= 2 {
                menu_defaults
                    .entry(parts[0].to_string())
                    .or_insert_with(|| parts[1].to_string());
            }
            let _ = value;
            continue;
        }

        insert_prop(&mut props, rest, value);
    }

    // Arduino-Pico exposes filesystem layouts through its `flash` menu, while
    // PlatformIO selects them with the derived `board_build.filesystem_size`
    // setting. Resolve that setting from the board's own fs_start/fs_end
    // metadata so no flash geometry is duplicated in fbuild.
    if !menu_overrides.contains_key("flash") {
        if let Some(filesystem_size) = menu_overrides.get("filesystem_size") {
            let option = flash_menu_option_for_filesystem_size(
                &content,
                &prefix,
                filesystem_size,
                menu_overrides.get("flash_size").map(String::as_str),
                menu_defaults.get("flash").map(String::as_str),
            );
            if let Some(option) = option {
                menu_defaults.insert("flash".to_string(), option);
            }
        }
    }

    for (menu, option) in menu_overrides {
        if !option.trim().is_empty() {
            menu_defaults.insert(menu.clone(), option.trim().to_string());
        }
    }

    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let Some((key, value)) = trimmed.split_once('=') else {
            continue;
        };
        let Some(rest) = key.strip_prefix(&prefix) else {
            continue;
        };
        let Some(menu_rest) = rest.strip_prefix("menu.") else {
            continue;
        };
        let parts = menu_rest.split('.').collect::<Vec<_>>();
        if parts.len() < 4 {
            continue;
        }
        let menu = parts[0];
        let option = parts[1];
        let Some(selected) = menu_defaults.get(menu) else {
            continue;
        };
        if selected != option {
            continue;
        }
        let prop_key = parts[2..].join(".");
        insert_prop(&mut props, &prop_key, value);
    }

    if props.is_empty() {
        return None;
    }

    let substitutions = props
        .iter()
        .map(|(key, value)| (format!("{{build.{key}}}"), value.clone()))
        .collect::<Vec<_>>();
    for value in props.values_mut() {
        for (needle, replacement) in &substitutions {
            if !replacement.is_empty() {
                *value = value.replace(needle, replacement);
            }
        }
    }

    Some(props)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_load_board_props_with_default_menus() {
        let tmp = tempfile::TempDir::new().unwrap();
        let boards_txt = tmp.path().join("boards.txt");
        std::fs::write(
            &boards_txt,
            "\
demo.build.board=DEMO
demo.build.flash_flags=-DFLASHMODE_DIO
demo.menu.speed.fast=Fast
demo.menu.speed.fast.build.f_cpu=160000000L
demo.menu.speed.slow=Slow
demo.menu.speed.slow.build.f_cpu=80000000L
demo.menu.mode.default=Default
demo.menu.mode.default.build.usb_product={build.board}
",
        )
        .unwrap();

        let props = load_board_props_with_default_menus(&boards_txt, "demo").unwrap();
        assert_eq!(props.get("board").map(String::as_str), Some("DEMO"));
        assert_eq!(props.get("f_cpu").map(String::as_str), Some("160000000L"));
        assert_eq!(
            props.get("flash_flags").map(String::as_str),
            Some("-DFLASHMODE_DIO")
        );
        assert_eq!(props.get("usb_product").map(String::as_str), Some("DEMO"));
    }

    #[test]
    fn test_load_board_props_applies_menu_override() {
        let tmp = tempfile::TempDir::new().unwrap();
        let boards_txt = tmp.path().join("boards.txt");
        std::fs::write(
            &boards_txt,
            "\
demo.menu.ipbtstack.ipv4=IPv4 only
demo.menu.ipbtstack.ipv4.build.libpicow=libipv4.a
demo.menu.ipbtstack.ipv4btcble=IPv4 Bluetooth
demo.menu.ipbtstack.ipv4btcble.build.libpicow=libipv4-bt.a
demo.menu.ipbtstack.ipv4btcble.build.libpicowdefs=-DENABLE_BLE=1
",
        )
        .unwrap();

        let props = load_board_props_with_menu_overrides(
            &boards_txt,
            "demo",
            &HashMap::from([(String::from("ipbtstack"), String::from("ipv4btcble"))]),
        )
        .unwrap();

        assert_eq!(
            props.get("libpicow").map(String::as_str),
            Some("libipv4-bt.a")
        );
        assert_eq!(
            props.get("libpicowdefs").map(String::as_str),
            Some("-DENABLE_BLE=1")
        );
    }

    #[test]
    fn test_load_board_props_maps_filesystem_size_to_flash_menu() {
        let tmp = tempfile::TempDir::new().unwrap();
        let boards_txt = tmp.path().join("boards.txt");
        std::fs::write(
            &boards_txt,
            "\
demo.menu.flash.4194304_0=4MB (no FS)
demo.menu.flash.4194304_0.upload.maximum_size=4190208
demo.menu.flash.4194304_0.build.flash_total=4194304
demo.menu.flash.4194304_0.build.flash_length=4190208
demo.menu.flash.4194304_0.build.fs_start=272621568
demo.menu.flash.4194304_0.build.fs_end=272621568
demo.menu.flash.4194304_524288=4MB (Sketch: 3580KB, FS: 512KB)
demo.menu.flash.4194304_524288.upload.maximum_size=3661824
demo.menu.flash.4194304_524288.build.flash_total=4194304
demo.menu.flash.4194304_524288.build.flash_length=3661824
demo.menu.flash.4194304_524288.build.fs_start=272097280
demo.menu.flash.4194304_524288.build.fs_end=272621568
",
        )
        .unwrap();

        let props = load_board_props_with_menu_overrides(
            &boards_txt,
            "demo",
            &HashMap::from([(String::from("filesystem_size"), String::from("0.5m"))]),
        )
        .unwrap();

        assert_eq!(
            props.get("maximum_size").map(String::as_str),
            Some("3661824")
        );
        assert_eq!(
            props.get("flash_total").map(String::as_str),
            Some("4194304")
        );
        assert_eq!(
            props.get("flash_length").map(String::as_str),
            Some("3661824")
        );
        assert_eq!(props.get("fs_start").map(String::as_str), Some("272097280"));
        assert_eq!(props.get("fs_end").map(String::as_str), Some("272621568"));
    }

    #[test]
    fn test_load_board_props_matches_filesystem_and_total_flash_sizes() {
        let tmp = tempfile::TempDir::new().unwrap();
        let boards_txt = tmp.path().join("boards.txt");
        std::fs::write(
            &boards_txt,
            "\
demo.menu.flash.4194304_2097152=4MB (FS: 2MB)
demo.menu.flash.4194304_2097152.build.flash_total=4194304
demo.menu.flash.4194304_2097152.build.fs_start=270524416
demo.menu.flash.4194304_2097152.build.fs_end=272621568
demo.menu.flash.8388608_2097152=8MB (FS: 2MB)
demo.menu.flash.8388608_2097152.upload.maximum_size=6287360
demo.menu.flash.8388608_2097152.build.flash_total=8388608
demo.menu.flash.8388608_2097152.build.fs_start=270524416
demo.menu.flash.8388608_2097152.build.fs_end=272621568
demo.menu.flash.16777216_2097152=16MB (FS: 2MB)
demo.menu.flash.16777216_2097152.build.flash_total=16777216
demo.menu.flash.16777216_2097152.build.fs_start=270524416
demo.menu.flash.16777216_2097152.build.fs_end=272621568
",
        )
        .unwrap();

        let props = load_board_props_with_menu_overrides(
            &boards_txt,
            "demo",
            &HashMap::from([
                (String::from("filesystem_size"), String::from("2m")),
                (String::from("flash_size"), String::from("8m")),
            ]),
        )
        .unwrap();

        assert_eq!(
            props.get("maximum_size").map(String::as_str),
            Some("6287360")
        );
        assert_eq!(
            props.get("flash_total").map(String::as_str),
            Some("8388608")
        );

        let content = std::fs::read_to_string(&boards_txt).unwrap();
        assert_eq!(
            flash_menu_option_for_filesystem_size(
                &content,
                "demo.",
                "2m",
                Some("8m"),
                Some("4194304_2097152"),
            ),
            Some(String::from("8388608_2097152"))
        );
        assert_eq!(
            flash_menu_option_for_filesystem_size(
                &content,
                "demo.",
                "2m",
                None,
                Some("4194304_2097152"),
            ),
            Some(String::from("4194304_2097152"))
        );
    }
}
