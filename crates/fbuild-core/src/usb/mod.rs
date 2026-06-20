//! USB VID:PID → human-readable vendor/product name resolution.
//!
//! Three resolution tiers, queried in order:
//!
//! 1. **Bundled** — the [`usb-ids`](https://crates.io/crates/usb-ids) crate,
//!    compiled in at build time as a `phf` perfect-hash table. Zero IO, zero
//!    allocations for the lookup itself. Tracks the upstream
//!    `linux-usb.org` snapshot the crate was published against.
//! 2. **Online overlay** — an optional `{ "VVVV:PPPP": {vendor, product} }`
//!    JSON map loaded at runtime (typically from a daemon-managed cache file
//!    that mirrors the `online-data` branch of this repo). The overlay
//!    provides newly-assigned VID/PID pairs that the bundled snapshot
//!    doesn't yet know about.
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
pub mod resolver;

pub use data::{install_online_cache, MANIFEST_URL, USB_VID_JSON_URL};
pub use resolver::{pretty, resolve, resolve_bundled, try_resolve, UsbInfo};
