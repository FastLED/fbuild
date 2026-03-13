//! Package management: toolchain resolution, library downloads, caching.
//!
//! Handles URL-based package management with parallel download pipeline.

pub mod downloader;
pub mod library;
pub mod toolchain;
