//! `.lnk` resource pointers — JSON manifests that point at remotely-hosted
//! binary blobs, fetched at build time and content-addressed by sha256.
//!
//! See `format.rs` for the schema and `README.md` for design rationale.
//!
//! ## Pipeline
//!
//! ```text
//!   .lnk file (in source tree)
//!         │
//!         ▼
//!     [scanner]  walks tree, parses every *.lnk
//!         │
//!         ▼
//!     [resolver] cache lookup by sha256; miss → download; verify
//!         │
//!         ▼
//!   [materializer] hardlink/copy cached blob into build/resources/<rel>
//!         │
//!         ▼
//!   downstream build steps (objcopy, embed_files, ...) consume the
//!   materialized file as if it had been in the source tree all along.
//! ```

pub mod embed;
pub mod format;
pub mod materialize;
pub mod resolver;
pub mod scanner;

pub use embed::{expand_lnk_entries, has_lnk_extension, materialize_lnk_entry};
pub use format::{ExtractMode, LnkFile};
pub use materialize::{materialize_all, materialize_one, MaterializedLnk};
pub use resolver::{resolve, ResolvedBlob};
pub use scanner::{scan_for_lnk, DiscoveredLnk};
