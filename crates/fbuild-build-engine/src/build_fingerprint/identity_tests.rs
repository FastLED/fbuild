//! The no-op fingerprint must cover fbuild itself (FastLED/fbuild#2036).
//!
//! fbuild adds compiler and linker flags of its own (MCU config JSONs, the
//! release profile, built-in defaults) that the project's inputs cannot see.
//! Before #2036 an fbuild upgrade that changed them (#2028 dropped nRF52
//! `-flto`) left "No-op fingerprint matched" reusing stale artifacts.

use super::{fbuild_build_identity, hash_build_inputs};

fn flags(values: &[&str]) -> Vec<String> {
    values.iter().map(|v| (*v).to_string()).collect()
}

#[test]
fn fbuild_identity_change_invalidates_metadata_hash() {
    let config = flags(&["-DUSER=1"]);
    let old = hash_build_inputs(&"platform-metadata", &config, "2.5.46+git.aaaaaaaaaaaa").unwrap();
    let new = hash_build_inputs(&"platform-metadata", &config, "2.5.47+git.aaaaaaaaaaaa").unwrap();
    let rebuilt =
        hash_build_inputs(&"platform-metadata", &config, "2.5.46+git.bbbbbbbbbbbb").unwrap();

    assert_ne!(old, new, "a new fbuild version must force a rebuild");
    assert_ne!(
        old, rebuilt,
        "a different fbuild commit must force a rebuild"
    );
}

#[test]
fn unchanged_inputs_keep_the_warm_build_hash() {
    let config = flags(&["-DUSER=1"]);
    let identity = fbuild_build_identity();
    assert_eq!(
        hash_build_inputs(&"platform-metadata", &config, &identity).unwrap(),
        hash_build_inputs(&"platform-metadata", &config, &identity).unwrap(),
        "the same fbuild and the same inputs must still no-op"
    );
}

#[test]
fn effective_flags_still_invalidate_with_identity() {
    let identity = fbuild_build_identity();
    let a = hash_build_inputs(&"platform-metadata", &flags(&["-DUSER=1"]), &identity).unwrap();
    let b = hash_build_inputs(&"platform-metadata", &flags(&["-DUSER=2"]), &identity).unwrap();
    assert_ne!(a, b);
}

#[test]
fn identity_carries_the_workspace_version() {
    assert!(fbuild_build_identity().starts_with(&format!("{}+git.", env!("CARGO_PKG_VERSION"))));
}
