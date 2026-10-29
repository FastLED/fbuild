//! Tests for [`crate::debug_prefix_map`].
//!
//! Raw `std::path::PathBuf` is banned outside the legacy allowlist, so these
//! tests rely on `Into` inference for the layout constructors and `Path` for
//! the free function.

use std::path::Path;

use fbuild_core::BuildProfile;

use crate::{BuildLayout, debug_prefix_map_args};

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
    assert_eq!(
        layout.debug_prefix_map_args(),
        vec![
            "-fdebug-prefix-map=/work/sketch=.".to_string(),
            "-fdebug-prefix-map=/tmp/short-build-dir/uno/quick=fbuild-build".to_string(),
        ]
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
