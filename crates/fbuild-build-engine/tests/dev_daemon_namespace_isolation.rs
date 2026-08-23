//! The dev daemon-identity stamp actually isolates the compile daemon
//! (FastLED/fbuild#1285).
//!
//! `fbuild-paths` derives a per-checkout stamp and exports it as
//! `ZCCACHE_DAEMON_NAMESPACE`; zccache folds that value into the IPC endpoint
//! its daemons rendezvous on. Neither half can prove the other works — the
//! producer lives in a crate that does not depend on zccache, and the
//! consumer is a pinned external dependency — so the contract *between* them
//! was asserted only in prose, and the prose was wrong: both #1285's tracking
//! comment and `fbuild_paths::dev_daemon_namespace`'s module doc claimed the
//! export was inert until fbuild repinned zccache. It is not; it has been
//! live since the stamp landed.
//!
//! This test is the place that can tell. It lives in `fbuild-build-engine`
//! because that is the crate depending on both sides. A zccache repin that
//! silently dropped endpoint namespacing would take the `displace-stale` war
//! from zackees/soldr#2352 with it, and nothing else in the tree would
//! notice.

use fbuild_paths::dev_daemon_namespace::ZCCACHE_DAEMON_NAMESPACE_ENV;

/// One test, not three: the variable is process-global, so parallel cases
/// would race each other's `set_var`.
#[test]
fn the_exported_stamp_changes_the_zccache_daemon_endpoint() {
    // SAFETY: this test binary contains one test, so no peer thread can
    // observe the process-wide environment change.
    unsafe { std::env::remove_var(ZCCACHE_DAEMON_NAMESPACE_ENV) };
    let bare = zccache::ipc::default_endpoint();

    unsafe { std::env::set_var(ZCCACHE_DAEMON_NAMESPACE_ENV, "2.5.0-aaaaaaaaaaaaaaaa") };
    let first = zccache::ipc::default_endpoint();

    unsafe { std::env::set_var(ZCCACHE_DAEMON_NAMESPACE_ENV, "2.5.0-bbbbbbbbbbbbbbbb") };
    let second = zccache::ipc::default_endpoint();

    unsafe { std::env::remove_var(ZCCACHE_DAEMON_NAMESPACE_ENV) };
    let bare_again = zccache::ipc::default_endpoint();

    // The property that matters: two checkouts with different stamps do not
    // meet on one pipe. Without this, each displaces the other as
    // "stale-version" on every invocation and the compile daemon wedges.
    assert_ne!(
        first, second,
        "two stamps must rendezvous on two different endpoints"
    );
    assert_ne!(
        first, bare,
        "a stamped endpoint must differ from the unstamped one"
    );

    // An unset stamp must keep the historical endpoint, or release builds
    // would silently move off the daemon they share on upgrade — the
    // single-daemon-on-upgrade semantics #1285 deliberately preserves for
    // non-dev invocations.
    assert_eq!(
        bare, bare_again,
        "clearing the stamp must restore the original endpoint exactly"
    );

    // The stamp is expected to appear in the endpoint rather than merely
    // perturb a hash of it: an operator reading `\\.\pipe\...` or a socket
    // path should be able to see which checkout owns the daemon.
    assert!(
        second.contains("bbbbbbbbbbbbbbbb"),
        "the stamp should be legible in the endpoint, got {second}"
    );
}
