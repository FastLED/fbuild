//! USB VID:PID → human-readable vendor/product name resolution.
//!
//! Three resolution tiers, queried in order:
//!
//! 1. **Online overlay** — an optional `{ "VVVV:PPPP": {vendor, product} }`
//!    JSON map loaded at runtime (typically from a daemon-managed cache
//!    file that mirrors the `online-data` branch of this repo). This is
//!    the richest source — it has both vendor AND product names — and is
//!    queried first.
//! 2. **Embedded vendor archive** — a 22 KB `tar.zst` blob compiled in
//!    via `include_bytes!` (see [`embedded`]). Vendor names only — for
//!    VIDs the overlay doesn't carry, we resolve the vendor offline and
//!    synthesize `"Device 0xPPPP"` as the product placeholder. Per-PID
//!    detail is intentionally not bundled — clients can hit the
//!    SQLite-over-HTTP database on the `www` branch for that.
//! 3. **Fallback** — synthetic `"Unknown vendor 0xVVVV"` placeholder so
//!    callers can always print something deterministic.
//!
//! See [`resolve`] (best-effort, never `None`), [`try_resolve`] (returns
//! `None` if both real tiers miss), and [`pretty`] (formatted as
//! `"vendor product (VVVV:PPPP)"` for connect / scan / `device list` log
//! lines).
//!
//! The daemon calls [`install_online_cache`] at startup with the path to
//! the locally-cached `usb-vid.json`. The CLI / nightly workflow keeps
//! that file in sync with the manifest URL exposed by the `online-data`
//! branch — see [`MANIFEST_URL`] and [`USB_VID_JSON_URL`].

pub mod data;
pub mod embedded;
pub mod resolver;

pub use data::{
    install_online_cache, install_online_cache_proto_zstd, try_install_online_cache,
    try_install_online_cache_proto_zstd, MANIFEST_URL, USB_VIDS_PROTO_ZSTD_URL, USB_VID_JSON_URL,
};
pub use embedded::vendor_name as embedded_vendor_name;
pub use resolver::{pretty, resolve, resolve_bundled, try_resolve, UsbInfo};
