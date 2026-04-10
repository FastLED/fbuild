use std::collections::HashMap;
use std::path::Path;

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
}
