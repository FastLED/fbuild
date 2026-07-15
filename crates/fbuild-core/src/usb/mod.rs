//! USB VID:PID → human-readable vendor/product name resolution.
//!
//! Two production resolution tiers, queried in order:
//!
//! 1. **FastLED/boards cache** — the verified published USB identity
//!    artifact loaded by the daemon. This is the only production device
//!    catalogue.
//! 2. **Fallback** — synthetic `"Unknown vendor 0xVVVV"` placeholder so
//!    callers can always print something deterministic.
//!
//! See [`resolve`] (best-effort, never `None`), [`try_resolve`] (returns
//! `None` if both real tiers miss), and [`pretty`] (formatted as
//! `"vendor product (VVVV:PPPP)"` for connect / scan / `device list` log
//! lines).
//!
//! Embedded vendor/device archives are available only in unit-test builds.
//! Production never silently falls back to compiled USB identity data.

pub mod data;
#[cfg(test)]
pub mod embedded;
pub mod profiles;
pub mod resolver;

pub use data::{
    install_online_cache, install_online_cache_proto_zstd, populate_online_cache_from_paths,
    populate_online_cache_from_paths_and_urls, try_install_online_cache,
    try_install_online_cache_proto_zstd, MANIFEST_URL, ONLINE_CACHE_TTL_SECS,
    USB_VIDS_PROTO_ZSTD_URL, USB_VID_JSON_URL,
};
#[cfg(test)]
pub use embedded::vendor_name as embedded_vendor_name;
#[cfg(test)]
pub use resolver::resolve_bundled;
pub use resolver::{pretty, resolve, try_resolve, UsbInfo};
