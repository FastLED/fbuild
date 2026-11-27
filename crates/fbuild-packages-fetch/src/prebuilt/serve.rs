//! The serving decision for one exact identity (#1882): prebuilt, or source.
//!
//! [`obtain`] answers "can this tree be produced?" with an error when it cannot.
//! A build must never see that error: a prebuilt artifact is an optimization,
//! and failing the compile because the optimization is unavailable turns a
//! server-side problem into a user-visible build break. This cell owns the
//! decision the plan calls *source fallback with one retry after a hit fails*:
//!
//! - a transient failure (a transport error, a short body, an integrity
//!   mismatch, a write that failed) is retried **once**; the fetcher keeps its
//!   partial bytes, so the retry resumes rather than restarts, and the
//!   extraction staging directory is unique per attempt, so a retry can never
//!   inherit debris from the previous one;
//! - a deterministic failure (an invalid identity field, an archive the
//!   descriptor can never match, a flavor v1 refuses, a foreign tree already at
//!   the identity's path) is **not** retried — a second identical attempt can
//!   only fail identically and cost another download;
//! - every path that does not serve a tree returns [`Served::Source`] carrying
//!   one line that names each attempt, so the caller can print the reason and
//!   compile from source. No error escapes this function at all.
//!
//! Opt-in: no build path calls this yet — the call site that would consult the
//! catalog and then this ladder is a later cell. The tests prove the ladder
//! itself against a real HTTP server, including the retry: the "retried once"
//! case fails when the retry is removed.
use super::{Asset, ExtractError, FetchError, Fetcher, ObtainError, Obtained, Query, obtain};
use std::path::Path;

/// Total attempts for one identity: the initial one plus exactly one retry.
pub const ATTEMPTS: u32 = 2;

/// What one query resolved to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Served {
    /// The tree is complete on disk (fresh, or reused from an earlier build).
    Prebuilt(Obtained),
    /// Prebuilt could not serve this identity; compile from source instead.
    Source(Fallback),
}

/// Why this build compiles from source, and how hard prebuilt was tried.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fallback {
    /// Every attempt's failure, in order, `;`-separated — one printable line.
    pub reason: String,
    /// How many times prebuilt was asked. `1` for a deterministic refusal.
    pub attempts: u32,
}

impl Fallback {
    /// One honest line for a build log; the caller decides the level.
    pub fn notice(&self) -> String {
        format!(
            "prebuilt: unavailable after {} attempt(s), compiling framework from source ({})",
            self.attempts, self.reason
        )
    }
}

/// Serve the tree for `query`, or fall back to a source build.
///
/// `fetcher` decides where verified archives are kept; `cache_root` is the
/// cache the tree is extracted into (see [`tree_root`](super::tree_root)).
pub async fn serve(cache_root: &Path, query: &Query, asset: &Asset, fetcher: &Fetcher) -> Served {
    let mut reasons: Vec<String> = Vec::new();
    let mut attempt = 0;
    while attempt < ATTEMPTS {
        attempt += 1;
        match obtain(cache_root, query, asset, fetcher).await {
            Ok(obtained) => return Served::Prebuilt(obtained),
            Err(error) => {
                let retry = worth_retrying(&error);
                reasons.push(format!("attempt {attempt}: {error}"));
                if !retry {
                    break;
                }
            }
        }
    }
    Served::Source(Fallback {
        reason: reasons.join("; "),
        attempts: attempt,
    })
}

/// Whether a second identical attempt could plausibly produce a tree.
///
/// The answer is about the *failure*, not the error's severity: anything that
/// describes bytes in flight or on disk is worth one retry (the partial is
/// kept, so the retry resumes); anything that describes a permanent mismatch
/// between the catalog value and this client is not.
fn worth_retrying(error: &ObtainError) -> bool {
    match error {
        ObtainError::Fetch(error) => match error {
            FetchError::Transport { .. } | FetchError::Exhausted { .. } => true,
            FetchError::Invalid(_) | FetchError::TooLarge { .. } => false,
        },
        ObtainError::Extract(error) => match error {
            ExtractError::Integrity { .. }
            | ExtractError::Archive(_)
            | ExtractError::Io { .. }
            | ExtractError::Join(_) => true,
            ExtractError::Invalid(_)
            | ExtractError::Unsupported(_)
            | ExtractError::Entry(_)
            | ExtractError::EntryType(_, _)
            | ExtractError::TooLarge { .. }
            | ExtractError::TooManyEntries { .. }
            | ExtractError::Occupied { .. } => false,
        },
        // The identity itself is wrong (never fixable by retrying) and a
        // conflicting tree belongs to somebody else (the LRU evicts it).
        ObtainError::InvalidFingerprint(_)
        | ObtainError::InvalidComponent { .. }
        | ObtainError::IdentityConflict { .. } => false,
    }
}

#[cfg(test)]
#[path = "serve_tests.rs"]
mod tests;
