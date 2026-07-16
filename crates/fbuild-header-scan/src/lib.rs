//! `#include` scanner and transitive include-graph walker.
//!
//! The scanner is a pure function from source text to a list of `IncludeRef`s.
//! The walker takes a seed set of source files and an ordered list of search
//! paths, resolves each `#include`, and returns the transitive closure of
//! reached files. Both are independent of fbuild infrastructure so they are
//! independently testable and reusable.

mod scanner;
mod walker;

pub use scanner::{IncludeKind, IncludeRef, Span, active_defines, scan, scan_active};
pub use walker::{
    WalkResult, WalkState, walk, walk_active, walk_with_state, walk_with_state_active,
};

/// Bumped whenever the scanner output shape changes. Mixed into cache keys so a
/// scanner change invalidates memoized library-selection results.
pub const SCANNER_VERSION: u32 = 1;
