//! Tests for [`crate::debug_prefix_map`].
//!
//! Raw `std::path::PathBuf` is banned outside the legacy allowlist, so these
//! tests rely on `Into` inference for the layout constructors and `Path` for
//! the free function.

use std::path::Path;

use fbuild_core::BuildProfile;

use crate::{BuildLayout, debug_prefix_map_args, portable_prefix_map_args};

#[test]
fn debug_prefix_map_covers_a_build_dir_inside_the_project() {
    let layout = BuildLayout::new(
        "/work/sketch".into(),
        "esp32dev".into(),
        BuildProfile::Release,
    );
    assert_eq!(
        layout.debug_prefix_map_args(),
        vec!["-fdebug-prefix-map=/work/sketch=.".to_string()]
    );
}

#[test]
fn debug_prefix_map_covers_an_external_build_root() {
    let layout = BuildLayout::new("/work/sketch".into(), "uno".into(), BuildProfile::Quick)
        .with_override_root(Some("/tmp/short-build-dir".into()));
    // Rendered paths are host-specific (Windows normalises to `\` and the
    // override root comes back with a leading separator), so assert the MEANING
    // of the second flag instead of string-matching a separator style.
    let args = layout.debug_prefix_map_args();
    assert_eq!(args.len(), 2, "{args:?}");
    assert_eq!(args[0], "-fdebug-prefix-map=/work/sketch=.");
    let external = &args[1];
    assert!(
        external.starts_with("-fdebug-prefix-map=") && external.ends_with("=fbuild-build"),
        "{external}"
    );
    let mapped = external
        .trim_start_matches("-fdebug-prefix-map=")
        .trim_end_matches("=fbuild-build");
    assert!(
        mapped.contains("short-build-dir"),
        "the override root must be mapped, got {mapped}"
    );
    assert!(
        Path::new(mapped).ends_with(Path::new("uno").join("quick")),
        "the mapped root must be <override>/<target>/<profile>, got {mapped}"
    );
}

/// A build dir that *is* the project dir is already covered, so only one
/// flag is emitted.
#[test]
fn debug_prefix_map_skips_a_build_dir_equal_to_the_project() {
    let project = Path::new("/work/sketch");
    assert_eq!(
        debug_prefix_map_args(project, project),
        vec!["-fdebug-prefix-map=/work/sketch=.".to_string()]
    );
}

/// The point of the flags: the remapped *target* is the same no matter
/// where the project lives, so the objects compare equal.
#[test]
fn debug_prefix_map_targets_do_not_depend_on_the_project_path() {
    let args_for = |project: &str| {
        BuildLayout::new(project.into(), "esp32dev".into(), BuildProfile::Release)
            .debug_prefix_map_args()
    };
    let a = args_for("/home/one/blink");
    let b = args_for("/home/two/other-blink");
    assert_eq!(a.len(), b.len());
    for (left, right) in a.iter().zip(&b) {
        let target = |flag: &str| flag.rsplit_once('=').unwrap().1.to_string();
        assert_eq!(target(left), target(right), "{left} vs {right}");
    }
}

/// The channels a shared compiler cache requires before it will reuse an
/// object across project directories (zccache's `cc_prefix_maps_cover_root`
/// needs file + macro + debug), in emission order.
const PORTABLE_CHANNELS: [&str; 3] = [
    "-ffile-prefix-map",
    "-fmacro-prefix-map",
    "-fdebug-prefix-map",
];

#[test]
fn portable_prefix_map_covers_every_channel_for_the_project_root() {
    let args = portable_prefix_map_args(Path::new("/work/sketch"), Path::new("/work/sketch"));
    assert_eq!(
        args,
        PORTABLE_CHANNELS
            .iter()
            .map(|channel| format!("{channel}=/work/sketch=."))
            .collect::<Vec<_>>()
    );
}

#[test]
fn portable_prefix_map_covers_an_external_build_root_on_every_channel() {
    let args = portable_prefix_map_args(Path::new("/work/sketch"), Path::new("/tmp/out"));
    assert_eq!(args.len(), 6, "{args:?}");
    for channel in PORTABLE_CHANNELS {
        assert!(
            args.iter()
                .any(|arg| arg == &format!("{channel}=/work/sketch=.")),
            "{channel} must cover the project root: {args:?}"
        );
        assert!(
            args.iter()
                .any(|arg| arg == &format!("{channel}=/tmp/out=fbuild-build")),
            "{channel} must cover the external build root: {args:?}"
        );
    }
}

/// The whole point of the trio: the remapped *targets* never depend on where
/// the project lives, so two checkouts of one sketch produce one cache key.
#[test]
fn portable_prefix_map_targets_do_not_depend_on_the_project_path() {
    let args_for = |project: &str| portable_prefix_map_args(Path::new(project), Path::new(project));
    let a = args_for("/home/one/blink");
    let b = args_for("/home/two/other-blink");
    assert_eq!(a.len(), b.len());
    for (left, right) in a.iter().zip(&b) {
        let (left_flag, left_target) = left.split_once('=').expect("flag=root=target");
        let (right_flag, right_target) = right.split_once('=').expect("flag=root=target");
        assert_eq!(left_flag, right_flag);
        assert_eq!(
            left_target.split_once('=').expect("root=target").1,
            right_target.split_once('=').expect("root=target").1
        );
    }
}
