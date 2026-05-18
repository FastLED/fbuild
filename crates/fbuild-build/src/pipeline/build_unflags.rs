//! `build_unflags` and debug-mode flag-cleanup helpers.
//!
//! Implements PlatformIO-compatible semantics for removing tokens from the
//! effective compile / link command, and the `build_type = debug` flag
//! transformation.

pub(super) fn apply_build_unflags(
    mut user_flags: Vec<String>,
    mut src_flags: Vec<String>,
    build_unflags: &[String],
) -> (Vec<String>, Vec<String>, Vec<String>) {
    if build_unflags.is_empty() {
        let all_src_flags = user_flags.iter().chain(src_flags.iter()).cloned().collect();
        return (user_flags, src_flags, all_src_flags);
    }

    remove_unflagged_tokens(&mut user_flags, build_unflags);
    remove_unflagged_tokens(&mut src_flags, build_unflags);
    let all_src_flags = user_flags.iter().chain(src_flags.iter()).cloned().collect();
    (user_flags, src_flags, all_src_flags)
}

pub(super) fn apply_debug_build_type(
    mut user_flags: Vec<String>,
    mut src_flags: Vec<String>,
    mut link_flags: Vec<String>,
    debug_build_flags: &[String],
) -> (Vec<String>, Vec<String>, Vec<String>) {
    cleanup_platformio_debug_scope(&mut user_flags);
    cleanup_platformio_debug_scope(&mut src_flags);
    cleanup_platformio_debug_scope(&mut link_flags);

    let mut compile_debug_flags = vec!["-D__PLATFORMIO_BUILD_DEBUG__".to_string()];
    compile_debug_flags.extend(debug_build_flags.iter().cloned());

    user_flags.extend(compile_debug_flags.iter().cloned());
    src_flags.extend(compile_debug_flags);

    let link_debug_flags: Vec<String> = debug_build_flags
        .iter()
        .filter(|flag| is_platformio_debug_link_flag(flag))
        .cloned()
        .collect();
    link_flags.extend(link_debug_flags);

    (user_flags, src_flags, link_flags)
}

/// Remove tokens listed in `build_unflags` from `flags` in place, using
/// PlatformIO-compatible semantics: exact token matches and flag-value
/// pair matches for options that take values (like `-include`, `-D`).
/// Public so platform compilers can apply it to the full effective flag
/// set — framework + toolchain + user — not just the user-facing scopes
/// already handled by `apply_build_unflags` in `BuildContext::new`.
/// See FastLED/fbuild#37.
pub fn remove_unflagged_tokens(flags: &mut Vec<String>, build_unflags: &[String]) {
    let mut i = 0;
    while i < build_unflags.len() {
        let token = &build_unflags[i];
        if flag_takes_value(token) && i + 1 < build_unflags.len() {
            remove_flag_value_pair(flags, token, &build_unflags[i + 1]);
            i += 2;
        } else {
            flags.retain(|flag| flag != token);
            i += 1;
        }
    }
}

fn remove_flag_value_pair(flags: &mut Vec<String>, option: &str, value: &str) {
    let mut filtered = Vec::with_capacity(flags.len());
    let mut i = 0;
    while i < flags.len() {
        let current = &flags[i];
        if current == option && i + 1 < flags.len() && flags[i + 1] == value {
            i += 2;
            continue;
        }
        filtered.push(current.clone());
        i += 1;
    }
    *flags = filtered;
}

fn cleanup_platformio_debug_scope(flags: &mut Vec<String>) {
    flags.retain(|flag| !is_platformio_debug_cleanup_flag(flag));
}

fn is_platformio_debug_cleanup_flag(flag: &str) -> bool {
    if flag == "-Os" || flag == "-g" {
        return true;
    }
    if flag.len() == 3 {
        let bytes = flag.as_bytes();
        if bytes[0] == b'-' && matches!(bytes[2], b'0' | b'1' | b'2' | b'3') {
            return matches!(bytes[1], b'O' | b'g');
        }
    }
    if flag.len() == 6 && flag.starts_with("-ggdb") {
        return matches!(flag.as_bytes()[5], b'0' | b'1' | b'2' | b'3');
    }
    false
}

fn is_platformio_debug_link_flag(flag: &str) -> bool {
    flag.starts_with("-O") || flag == "-g" || flag.starts_with("-g")
}

fn flag_takes_value(flag: &str) -> bool {
    matches!(
        flag,
        "-include"
            | "-imacros"
            | "-isystem"
            | "-iquote"
            | "-iprefix"
            | "-iwithprefix"
            | "-iwithprefixbefore"
            | "-Xlinker"
            | "-Wa"
            | "-Wl"
            | "-Wp"
            | "-L"
            | "-T"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_apply_build_unflags_removes_exact_tokens_from_global_and_src_flags() {
        let (user_flags, src_flags, all_src_flags) = apply_build_unflags(
            vec![
                "-Os".to_string(),
                "-DDEBUG".to_string(),
                "-Wall".to_string(),
            ],
            vec!["-DDEBUG".to_string(), "-Winvalid-pch".to_string()],
            &["-DDEBUG".to_string(), "-Os".to_string()],
        );

        assert_eq!(user_flags, vec!["-Wall"]);
        assert_eq!(src_flags, vec!["-Winvalid-pch"]);
        assert_eq!(all_src_flags, vec!["-Wall", "-Winvalid-pch"]);
    }

    #[test]
    fn test_apply_build_unflags_removes_common_option_value_pair() {
        let (user_flags, src_flags, all_src_flags) = apply_build_unflags(
            vec![
                "-include".to_string(),
                "config/common.h".to_string(),
                "-Wall".to_string(),
            ],
            vec![
                "-include".to_string(),
                "config/common.h".to_string(),
                "-Winvalid-pch".to_string(),
            ],
            &["-include".to_string(), "config/common.h".to_string()],
        );

        assert_eq!(user_flags, vec!["-Wall"]);
        assert_eq!(src_flags, vec!["-Winvalid-pch"]);
        assert_eq!(all_src_flags, vec!["-Wall", "-Winvalid-pch"]);
    }

    #[test]
    fn test_apply_debug_build_type_replaces_opt_flags_and_adds_debug_define() {
        let (user_flags, src_flags, link_flags) = apply_debug_build_type(
            vec!["-Os".to_string(), "-Wall".to_string()],
            vec!["-O2".to_string(), "-Winvalid-pch".to_string()],
            Vec::new(),
            &["-Og".to_string(), "-g2".to_string(), "-ggdb2".to_string()],
        );

        assert_eq!(
            user_flags,
            vec![
                "-Wall",
                "-D__PLATFORMIO_BUILD_DEBUG__",
                "-Og",
                "-g2",
                "-ggdb2"
            ]
        );
        assert_eq!(
            src_flags,
            vec![
                "-Winvalid-pch",
                "-D__PLATFORMIO_BUILD_DEBUG__",
                "-Og",
                "-g2",
                "-ggdb2"
            ]
        );
        assert_eq!(link_flags, vec!["-Og", "-g2", "-ggdb2"]);
    }

    #[test]
    fn test_debug_mode_then_build_unflags_can_remove_debug_flags_again() {
        let (user_flags, src_flags, mut link_flags) = apply_debug_build_type(
            vec!["-Os".to_string(), "-Wall".to_string()],
            vec!["-Winvalid-pch".to_string()],
            Vec::new(),
            &["-Og".to_string(), "-g2".to_string(), "-ggdb2".to_string()],
        );
        let (user_flags, src_flags, all_src_flags) =
            apply_build_unflags(user_flags, src_flags, &["-g2".to_string()]);
        remove_unflagged_tokens(&mut link_flags, &["-g2".to_string()]);

        assert_eq!(
            user_flags,
            vec!["-Wall", "-D__PLATFORMIO_BUILD_DEBUG__", "-Og", "-ggdb2"]
        );
        assert_eq!(
            src_flags,
            vec![
                "-Winvalid-pch",
                "-D__PLATFORMIO_BUILD_DEBUG__",
                "-Og",
                "-ggdb2"
            ]
        );
        assert_eq!(
            all_src_flags,
            vec![
                "-Wall",
                "-D__PLATFORMIO_BUILD_DEBUG__",
                "-Og",
                "-ggdb2",
                "-Winvalid-pch",
                "-D__PLATFORMIO_BUILD_DEBUG__",
                "-Og",
                "-ggdb2"
            ]
        );
        assert_eq!(link_flags, vec!["-Og", "-ggdb2"]);
    }
}
