//! Forward-compat alias for the HTTP client bridge.
//!
//! FastLED/fbuild#844 hoisted the shared `reqwest::Client` to
//! `fbuild_core::http`. This module is a re-export so existing
//! `use fbuild_packages::http::client;` call sites compile until the
//! Phase 2 migration sweep moves them to `fbuild_core::http::client()`.
//!
//! New code should import directly from `fbuild_core::http`. The
//! `ban_bare_reqwest` dylint forbids `reqwest::Client::new()` outside
//! the bridge.

pub use fbuild_core::http::{
    blocking_client, client, client_with_timeout, DEFAULT_CONNECT_TIMEOUT, DEFAULT_TIMEOUT,
};
