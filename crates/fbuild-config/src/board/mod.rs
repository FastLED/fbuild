//! Board configuration from boards.txt and built-in defaults.
//!
//! Supports:
//! - Loading from Arduino boards.txt format
//! - Built-in defaults for common boards
//! - Field overrides from platformio.ini board_build.* keys
//! - Preprocessor defines generation
//! - Include path resolution
//!
//! The module is split into:
//! - `types` – the public [`BoardConfig`] struct and supporting types
//! - `loaders` – `from_boards_txt` / `from_board_id` constructors and the
//!   `boards.txt` line parser
//! - `methods` – accessor / derivation methods on `BoardConfig`
//! - `db` – embedded JSON board database, alias resolution, default extraction
//!
//! The four submodules are private; only the types re-exported below
//! form the public API.

mod db;
mod loaders;
pub mod mcu_vid;
mod methods;
mod types;

#[cfg(test)]
mod tests;

#[cfg(test)]
mod tests_enriched_json;

#[cfg(test)]
mod tests_project_local;

#[cfg(test)]
mod tests_lpc8xx;

#[cfg(test)]
mod tests_usb_vid;

#[cfg(test)]
mod tests_common_board_vidpid;

pub use types::{BoardConfig, DebugToolMeta, Esp32QemuPsramConfig};
