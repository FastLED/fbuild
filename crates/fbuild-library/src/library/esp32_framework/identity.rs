//! The framework term of a prebuilt fingerprint (decision 8a, FastLED/fbuild#1882).
//!
//! A build's fingerprint already covers its board, toolchain and flags; this
//! module reads the identities the framework's components wrote **in staging**
//! and turns them into the one term that covers the framework bytes those flags
//! point at. The term is read from stamps only - never re-hashed from the
//! installed tree - so it is install-history independent: building another chip
//! into the same core tree adds that chip's component and leaves this one's
//! term unchanged.
//!
//! A tree with no identity produces `None`, and its caller keeps compiling from
//! source rather than certifying bytes nobody hashed.

use std::path::Path;

use fbuild_packages_fetch::component_id::{component_term, read_stamp};

use super::fs_utils::find_framework_root;
use super::libs;

/// Canonical identity of the framework installed at `install_dir`, or `None`
/// when it has none.
///
/// Required: the `core` stamp at the framework root and the `sdk-<variant>`
/// stamp of every variant this board links against (the MCU plus the board's
/// chip variant, FastLED/fbuild#1897). A required variant with no complete tree
/// or no stamp means the framework is either legacy, half-installed, or
/// half-merged, and the term is withheld.
///
/// The `mcu-<mcu>` skeleton is included when it carries a stamp. A platform
/// that ships no skeleton simply has no such stamp; skeletons are installed and
/// stamped by the same provisioning run that stamps the SDK variant, so a
/// stamped SDK tree with an unstamped skeleton can only be a tree whose merge
/// verification withdrew the stamp - which is the state `ensure_mcu_libs`
/// repairs, not one this reader certifies.
#[must_use]
pub(crate) fn framework_identity(
    install_dir: &Path,
    mcu: &str,
    sdk_variant: &str,
) -> Option<String> {
    let root = find_framework_root(install_dir);
    let mut identities = vec![read_stamp(&root, "core")?];
    let tools_dir = root.join("tools");
    for variant in libs::required_sdk_variants(mcu, sdk_variant) {
        let variant_dir = libs::installed_variant_dir(&tools_dir, variant)?;
        identities.push(read_stamp(&variant_dir, &format!("sdk-{variant}"))?);
    }
    if let Some(mcu_dir) = libs::installed_variant_dir(&tools_dir, mcu) {
        if let Some(skeleton) = read_stamp(&mcu_dir, &format!("mcu-{mcu}")) {
            identities.push(skeleton);
        }
    }
    component_term(&identities)
}
