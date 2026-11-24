//! Canonical prebuilt-core build fingerprint (decision 8, FastLED/fbuild#1882).
//!
//! A prebuilt framework-core object is a tar of the Arduino core's own compiled
//! translation units, valid only for one exact compiler invocation. The key that
//! addresses it is therefore a fingerprint over everything that can change those
//! bytes: the ABI schema of this hash, the toolchain, the framework's component
//! identities, the chip and SDK variant, the canonical command line of every
//! compiled unit, and the config/header digests those units saw.
//!
//! Two properties are load-bearing and are why this lives next to
//! [`super::identity`] instead of in the download layer:
//!
//! - **Install-history independence.** The framework term comes from the stamps
//!   the components wrote in staging, so a publisher with only `esp32c3`
//!   installed and a user who also built `esp32dev` agree on the fingerprint.
//! - **No identity, no serving.** A tree with no stamps - legacy, half-installed,
//!   half-merged, or patched in place by an older fbuild - yields `None`, and the
//!   caller compiles from source. Nothing here ever writes a stamp or re-hashes
//!   an installed tree on the build path.

use sha2::{Digest, Sha256};

use super::Esp32Framework;

/// Schema of the fingerprint below. Bump when a *field* changes meaning, or when
/// the hash framing changes: a fingerprint minted by another schema must never
/// address an object published under this one.
pub const ABI_SCHEMA: u32 = 1;

/// Domain tag, so this digest can never collide with another sha256 in the
/// workspace over the same bytes.
const DOMAIN: &[u8] = b"fbuild-prebuilt-core-fingerprint\0";

/// One compiled translation unit: a stable unit id and the command line that
/// compiles it, in invocation order.
///
/// The argv must already be *canonical* (project paths replaced by prefix-map
/// placeholders, no temporary directory names): this module cannot tell an
/// install-specific path from a meaningful one, and a fingerprint that moves
/// with the build directory can never be served to a second user.
#[derive(Debug, Clone, Copy)]
pub struct CompileUnit<'a> {
    /// Stable id of the unit, e.g. the source path relative to the core root.
    pub unit: &'a str,
    /// Full compiler command line, in the order the compiler receives it.
    pub argv: &'a [String],
}

/// A config/header file digest the units were compiled against, e.g.
/// `sdkconfig.h` or `esp32-hal.h`, named by a stable id.
#[derive(Debug, Clone, Copy)]
pub struct ConfigDigest<'a> {
    /// Stable id, e.g. the header path relative to the core root.
    pub id: &'a str,
    /// Lowercase hex sha256 of that header's bytes.
    pub sha256: &'a str,
}

/// Everything outside the install tree that changes the compiled core bytes.
pub struct CoreBuildInputs<'a> {
    /// Compiler ABI identity, e.g. `xtensa-esp32-elf-gcc 14.2.0_20260121`. A
    /// toolchain that self-reports the same version but different bytes must be
    /// given a different value here.
    pub compiler_abi: &'a str,
    /// Toolchain package name, e.g. `toolchain-xtensa-esp32s3`.
    pub toolchain_name: &'a str,
    /// Toolchain package version.
    pub toolchain_version: &'a str,
    /// MCU the core is compiled for, e.g. `esp32c3`.
    pub mcu: &'a str,
    /// SDK variant whose headers and libraries the units link against
    /// (`libs::required_sdk_variants`; the MCU's own variant in the common case).
    pub sdk_variant: &'a str,
    /// Every compiled unit. Order is canonicalised by unit id.
    pub units: &'a [CompileUnit<'a>],
    /// Every config/header digest the units were compiled against. Order is
    /// canonicalised by id.
    pub config: &'a [ConfigDigest<'a>],
}

/// A servable identity for one prebuilt core object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrebuiltCorePlan {
    fingerprint: String,
    mcu: String,
    sdk_variant: String,
    core_version: String,
}

impl PrebuiltCorePlan {
    /// The canonical fingerprint: the `<sha16>`/index key of a published object.
    #[must_use]
    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }

    /// MCU this plan's objects were compiled for.
    #[must_use]
    pub fn mcu(&self) -> &str {
        &self.mcu
    }

    /// SDK variant those objects were compiled against.
    #[must_use]
    pub fn sdk_variant(&self) -> &str {
        &self.sdk_variant
    }

    /// Framework (Arduino core) version they were compiled from.
    #[must_use]
    pub fn core_version(&self) -> &str {
        &self.core_version
    }
}

impl Esp32Framework {
    /// Plan a prebuilt core object for this installed framework, or `None` when
    /// it has no identity or the build inputs are incomplete.
    ///
    /// `None` is the ordinary answer for a legacy tree and for every caller that
    /// cannot supply canonical inputs; it always means "compile from source",
    /// never "guess". No stamp is written here.
    #[must_use]
    pub fn prebuilt_core_plan(&self, inputs: &CoreBuildInputs<'_>) -> Option<PrebuiltCorePlan> {
        let framework = self.framework_identity(inputs.mcu, inputs.sdk_variant)?;
        let argv = canonical_argv_digest(inputs.units)?;
        let config = canonical_config_digest(inputs.config)?;
        if [
            inputs.compiler_abi,
            inputs.toolchain_name,
            inputs.toolchain_version,
            inputs.mcu,
            inputs.sdk_variant,
        ]
        .iter()
        .any(|value| value.trim().is_empty())
        {
            return None;
        }

        let core_version = self.base.version.clone();
        let mut hash = Sha256::new();
        hash.update(DOMAIN);
        absorb(&mut hash, "schema", &ABI_SCHEMA.to_string());
        absorb(&mut hash, "core_version", &core_version);
        absorb(&mut hash, "compiler_abi", inputs.compiler_abi);
        absorb(&mut hash, "toolchain_name", inputs.toolchain_name);
        absorb(&mut hash, "toolchain_version", inputs.toolchain_version);
        absorb(&mut hash, "mcu", inputs.mcu);
        absorb(&mut hash, "sdk_variant", inputs.sdk_variant);
        absorb(&mut hash, "framework", &framework);
        absorb(&mut hash, "argv", &argv);
        absorb(&mut hash, "config", &config);

        Some(PrebuiltCorePlan {
            fingerprint: hex(&hash.finalize()),
            mcu: inputs.mcu.to_string(),
            sdk_variant: inputs.sdk_variant.to_string(),
            core_version,
        })
    }
}

/// Digest of the compiled units: unit id, then its argv in invocation order.
///
/// The unit list is sorted by id, so two planners that enumerate the same units
/// in a different order (a directory walk, a manifest, a second host) agree. An
/// empty list is `None`: a core object with no units would certify nothing.
#[must_use]
pub fn canonical_argv_digest(units: &[CompileUnit<'_>]) -> Option<String> {
    if units.is_empty() || units.iter().any(|unit| unit.unit.trim().is_empty()) {
        return None;
    }
    let mut units = units.to_vec();
    units.sort_by(|left, right| left.unit.cmp(right.unit));
    let mut hash = Sha256::new();
    hash.update(b"fbuild-prebuilt-core-argv\0");
    for unit in units {
        absorb(&mut hash, "unit", unit.unit);
        absorb(&mut hash, "argc", &unit.argv.len().to_string());
        for arg in unit.argv {
            absorb(&mut hash, "arg", arg);
        }
    }
    Some(hex(&hash.finalize()))
}

/// Digest of the config/header files the units were compiled against, sorted by
/// id. An empty list is `None`, for the same reason an empty unit list is.
#[must_use]
pub fn canonical_config_digest(config: &[ConfigDigest<'_>]) -> Option<String> {
    if config.is_empty()
        || config
            .iter()
            .any(|entry| entry.id.trim().is_empty() || entry.sha256.trim().is_empty())
    {
        return None;
    }
    let mut config = config.to_vec();
    config.sort_by(|left, right| left.id.cmp(right.id));
    let mut hash = Sha256::new();
    hash.update(b"fbuild-prebuilt-core-config\0");
    for entry in config {
        absorb(&mut hash, "id", entry.id);
        absorb(&mut hash, "sha256", entry.sha256);
    }
    Some(hex(&hash.finalize()))
}

/// Length-prefixed field, so no two field tuples can hash the same bytes by
/// moving a character across a boundary.
fn absorb(hash: &mut Sha256, label: &str, value: &str) {
    hash.update((label.len() as u64).to_le_bytes());
    hash.update(label.as_bytes());
    hash.update((value.len() as u64).to_le_bytes());
    hash.update(value.as_bytes());
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut out, byte| {
        let _ = write!(out, "{byte:02x}");
        out
    })
}
