//! `#include` scanner and transitive include-graph walker.
//!
//! The scanner is a pure function from source text to a list of `IncludeRef`s.
//! The walker takes a seed set of source files and an ordered list of search
//! paths, resolves each `#include`, and returns the transitive closure of
//! reached files. Both are independent of fbuild infrastructure so they are
//! independently testable and reusable.

mod scanner;
mod walker;

pub use scanner::{
    IncludeKind, IncludeRef, Span, active_defines, defined_macro_names, scan, scan_active,
    scan_active_with_known,
};
pub use walker::{
    WalkResult, WalkState, collect_defined_macro_names, walk, walk_active, walk_with_state,
    walk_with_state_active, walk_with_state_active_known,
};

/// Bumped whenever the scanner output shape changes. Mixed into cache keys so a
/// scanner change invalidates memoized library-selection results.
pub const SCANNER_VERSION: u32 = 1;
