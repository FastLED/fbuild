//! Tests for `adoptable_owner_port` (split from lib.rs for the 1000-LOC gate).

use super::{adoptable_owner_port, daemon_ownership};

fn claim(version: &str, mode: &str, cache: &str, port: u16) -> daemon_ownership::OwnerClaim {
    daemon_ownership::OwnerClaim {
        pid: std::process::id(),
        exe: fbuild_core::path::NormalizedPath::new("/x/fbuild-daemon"),
        version: version.to_string(),
        mode: mode.to_string(),
        cache_root_key: cache.to_string(),
        port,
    }
}

/// Two same-version builds that derive different default ports must
/// still find the live daemon that owns their shared cache root.
#[test]
fn owner_claim_port_is_adopted_for_same_version_identity() {
    let c = claim("2.5.44", "prod", "/c", 60903);
    assert_eq!(
        adoptable_owner_port(&c, "2.5.44", "prod", "/c", |_| true),
        Some(60903)
    );
}

#[test]
fn owner_claim_port_is_not_adopted_across_identity_or_when_dead() {
    let c = claim("2.5.44", "prod", "/c", 60903);
    assert_eq!(
        adoptable_owner_port(&c, "2.5.44", "prod", "/c", |_| false),
        None
    );
    assert_eq!(
        adoptable_owner_port(&c, "2.5.45", "prod", "/c", |_| true),
        None
    );
    assert_eq!(
        adoptable_owner_port(&c, "2.5.44", "dev", "/c", |_| true),
        None
    );
    assert_eq!(
        adoptable_owner_port(&c, "2.5.44", "prod", "/d", |_| true),
        None
    );
    let zero = claim("2.5.44", "prod", "/c", 0);
    assert_eq!(
        adoptable_owner_port(&zero, "2.5.44", "prod", "/c", |_| true),
        None
    );
}
