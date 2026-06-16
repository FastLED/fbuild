//! zccache K/V memoization for the [`crate::resolve`] function.
//!
//! The resolver is a pure function of (project sources, search paths, library
//! set, toolchain triple, framework version, scanner/LDF semantics). Computing
//! it costs ~tens of ms cold; restoring from a cache hit is sub-ms. On a warm
//! CI build this should round-trip through a small fbuild-owned file cache
//! instead of re-walking the framework `libraries/` tree.
//!
//! ## Cache key (Q9 from #205)
//!
//! `blake3(` of the following, concatenated with framing tags:
//!   - sorted seed-path + content-hash pairs,
//!   - sorted (lib_name, canonical_header_hash) pairs,
//!   - the ordered search-path list (verbatim — order is observable),
//!   - toolchain_triple,
//!   - framework_install_path,
//!   - framework_version,
//!   - [`SCANNER_VERSION`],
//!   - [`LDF_MODE_VERSION`].
//!
//! Bumping `SCANNER_VERSION` invalidates every cached entry whose include
//! grammar may have changed. Bumping `LDF_MODE_VERSION` invalidates every
//! entry whose 2-pass resolver semantics may have changed. Both are blunt
//! and intentional — partial migration of a malformed cache is worse than
//! a one-time recompute.

use std::path::{Path, PathBuf};

use fbuild_packages::library::FrameworkLibrary;
use prost::Message;

use crate::{resolve, Selection};

/// Bump when the scanner's lexical grammar changes in a way that could change
/// which `#include` directives it emits for the same source.
pub const SCANNER_VERSION: u32 = 1;

/// Bump when the resolver's 2-pass LDF semantics change (seed expansion,
/// attribution, convergence rule, etc.).
pub const LDF_MODE_VERSION: u32 = 1;

/// Namespace for the library-selection file cache.
pub const NAMESPACE: &str = "library-selection";

const CACHE_ENVELOPE_VERSION: u32 = 1;

/// Content-addressed cache key for library-selection memoization.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CacheKey([u8; 32]);

impl CacheKey {
    fn from_hash(hash: blake3::Hash) -> Self {
        Self(*hash.as_bytes())
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    pub fn to_hex(&self) -> String {
        let mut out = String::with_capacity(64);
        for byte in self.0 {
            use std::fmt::Write as _;
            let _ = write!(&mut out, "{byte:02x}");
        }
        out
    }
}

/// Minimal file-backed K/V cache for best-effort memoized resolver output.
///
/// This deliberately avoids a database engine. Values are deterministic and
/// disposable: corrupt, missing, or stale entries are treated as cache misses.
#[derive(Debug, Clone)]
pub struct FileKvStore {
    root: PathBuf,
}

impl FileKvStore {
    pub fn open<P: AsRef<Path>>(root: P) -> CacheResult<Self> {
        let root = root.as_ref().to_path_buf();
        std::fs::create_dir_all(&root)?;
        Ok(Self { root })
    }

    pub fn get(&self, namespace: &str, key: &CacheKey) -> CacheResult<Option<Vec<u8>>> {
        validate_namespace(namespace)?;
        let path = self.entry_path(namespace, key);
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(err) => return Err(err.into()),
        };
        let envelope = match CacheEnvelope::decode(bytes.as_slice()) {
            Ok(envelope) => envelope,
            Err(err) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %err,
                    "library-select cache: corrupt protobuf envelope; treating as miss"
                );
                return Ok(None);
            }
        };
        if envelope.schema_version != CACHE_ENVELOPE_VERSION {
            return Ok(None);
        }
        Ok(Some(envelope.payload))
    }

    pub fn put(&self, namespace: &str, key: &CacheKey, value: &[u8]) -> CacheResult<usize> {
        validate_namespace(namespace)?;
        let path = self.entry_path(namespace, key);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let envelope = CacheEnvelope {
            schema_version: CACHE_ENVELOPE_VERSION,
            payload: value.to_vec(),
        };
        let bytes = envelope.encode_to_vec();
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let tmp = path.with_extension(format!("tmp.{}.{}", std::process::id(), nonce));
        std::fs::write(&tmp, bytes)?;
        std::fs::rename(&tmp, &path).or_else(|err| {
            let _ = std::fs::remove_file(&path);
            std::fs::rename(&tmp, &path).map_err(|_| err)
        })?;
        Ok(value.len())
    }

    fn entry_path(&self, namespace: &str, key: &CacheKey) -> PathBuf {
        self.root
            .join(namespace)
            .join(format!("{}.pb", key.to_hex()))
    }
}

#[derive(Clone, PartialEq, Message)]
struct CacheEnvelope {
    #[prost(uint32, tag = "1")]
    schema_version: u32,
    #[prost(bytes, tag = "2")]
    payload: Vec<u8>,
}

pub type CacheResult<T> = Result<T, CacheError>;

#[derive(Debug, thiserror::Error)]
pub enum CacheError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("bad cache namespace")]
    BadNamespace,
}

fn validate_namespace(namespace: &str) -> CacheResult<()> {
    if !namespace.is_empty()
        && namespace
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    {
        Ok(())
    } else {
        Err(CacheError::BadNamespace)
    }
}

/// Inputs that vary independently of the (seeds, search_paths, libraries)
/// triple and must contribute to the cache key — toolchain + framework
/// identity. Held separately so the resolver call site (orchestrator) can
/// supply them once per build.
#[derive(Debug, Clone)]
pub struct CacheKeyInputs<'a> {
    /// Toolchain triple, e.g. `avr-unknown-none` or `xtensa-esp32-elf`.
    pub toolchain_triple: &'a str,
    /// On-disk install root of the framework (e.g. the Teensyduino root).
    pub framework_install_path: &'a Path,
    /// Framework version identifier (release string parsed from whatever
    /// version file the framework carries — `package.json`, `platform.txt`,
    /// or a `framework-name@version` line).
    pub framework_version: &'a str,
}

/// Result of [`resolve_cached`]. `from_cache` distinguishes hit from miss so
/// the caller can attribute build-time / log accordingly.
#[derive(Debug, Clone)]
pub struct CachedSelection {
    pub selection: Selection,
    pub key: CacheKey,
    pub from_cache: bool,
}

/// Compute the cache key for a (seeds, search_paths, libraries, inputs)
/// tuple. Pure function — no I/O beyond reading file bodies for hashing.
///
/// Errors only on filesystem failures while reading seeds or canonical
/// library headers; in either case we fall back to *not* hashing the
/// missing file and continue (a missing seed is the resolver's problem,
/// not the cache key's). Non-existence is recorded as the empty hash.
#[must_use]
pub fn cache_key(
    seeds: &[PathBuf],
    search_paths: &[PathBuf],
    libraries: &[FrameworkLibrary],
    inputs: &CacheKeyInputs<'_>,
) -> CacheKey {
    let mut h = blake3::Hasher::new();

    h.update(b"fbuild.library-select.v1\n");
    h.update(&SCANNER_VERSION.to_le_bytes());
    h.update(&LDF_MODE_VERSION.to_le_bytes());

    h.update(b"toolchain:");
    h.update(inputs.toolchain_triple.as_bytes());
    h.update(b"\n");

    h.update(b"framework_path:");
    h.update(inputs.framework_install_path.to_string_lossy().as_bytes());
    h.update(b"\n");

    h.update(b"framework_version:");
    h.update(inputs.framework_version.as_bytes());
    h.update(b"\n");

    // Seeds: sorted by path, each contributes (canonical_path, content_hash).
    let mut seed_pairs: Vec<(String, [u8; 32])> = seeds
        .iter()
        .map(|p| {
            let canon = std::fs::canonicalize(p).unwrap_or_else(|_| p.clone());
            let bytes = std::fs::read(&canon).unwrap_or_default();
            (
                canon.to_string_lossy().into_owned(),
                *blake3::hash(&bytes).as_bytes(),
            )
        })
        .collect();
    seed_pairs.sort_by(|a, b| a.0.cmp(&b.0));
    h.update(b"seeds:");
    h.update(&(seed_pairs.len() as u64).to_le_bytes());
    for (path, content) in &seed_pairs {
        h.update(&(path.len() as u64).to_le_bytes());
        h.update(path.as_bytes());
        h.update(content);
    }

    // Search paths: ORDER MATTERS (PIO resolves earlier paths first), so we
    // hash them in the order supplied — do not sort.
    h.update(b"search_paths:");
    h.update(&(search_paths.len() as u64).to_le_bytes());
    for p in search_paths {
        let s = p.to_string_lossy();
        h.update(&(s.len() as u64).to_le_bytes());
        h.update(s.as_bytes());
    }

    // Libraries: sorted by name. Content hash of the canonical header
    // (`<lib>/<src>/<name>.h` or first include_dir hit) catches the common
    // case of a lib's API header changing without bumping the framework
    // version. We deliberately do NOT hash every source file — Teensyduino
    // alone is ~500 files and PIO LDF doesn't either.
    let mut lib_pairs: Vec<(String, [u8; 32])> = libraries
        .iter()
        .map(|lib| (lib.name.clone(), canonical_header_hash(lib)))
        .collect();
    lib_pairs.sort_by(|a, b| a.0.cmp(&b.0));
    h.update(b"libraries:");
    h.update(&(lib_pairs.len() as u64).to_le_bytes());
    for (name, content) in &lib_pairs {
        h.update(&(name.len() as u64).to_le_bytes());
        h.update(name.as_bytes());
        h.update(content);
    }

    CacheKey::from_hash(h.finalize())
}

fn canonical_header_hash(lib: &FrameworkLibrary) -> [u8; 32] {
    // Try `<include_dir>/<lib_name>.h` for each of the lib's include dirs.
    // First hit wins; falls back to a hash of the empty string if the lib
    // doesn't expose its name as a header (rare — usually a pure
    // implementation-only lib).
    for dir in &lib.include_dirs {
        let candidate = dir.join(format!("{}.h", lib.name));
        if let Ok(bytes) = std::fs::read(&candidate) {
            return *blake3::hash(&bytes).as_bytes();
        }
    }
    *blake3::hash(b"").as_bytes()
}

/// Resolve with cache. On miss, computes the selection, stores it, and
/// returns it with `from_cache = false`. On hit, deserializes the stored
/// `Selection` and returns it with `from_cache = true`.
///
/// Cache lookups that fail decoding (corrupt entry, schema drift) fall
/// through to recomputation rather than propagating — a stale cache must
/// never poison a build. The tainted entry is overwritten by the fresh
/// computation's `put`.
///
/// Errors propagate only from the underlying [`FileKvStore`] backend
/// (filesystem or protobuf decode). Construction-time validation of the
/// namespace is impossible to fail because [`NAMESPACE`] is a const.
pub fn resolve_cached(
    seeds: &[PathBuf],
    search_paths: &[PathBuf],
    libraries: &[FrameworkLibrary],
    inputs: &CacheKeyInputs<'_>,
    store: &FileKvStore,
) -> CacheResult<CachedSelection> {
    let key = cache_key(seeds, search_paths, libraries, inputs);

    if let Some(bytes) = store.get(NAMESPACE, &key)? {
        match bincode::deserialize::<Selection>(&bytes) {
            Ok(selection) => {
                return Ok(CachedSelection {
                    selection,
                    key,
                    from_cache: true,
                });
            }
            Err(err) => {
                tracing::warn!(
                    key = %key.to_hex(),
                    error = %err,
                    "library-select cache: corrupt entry; recomputing"
                );
            }
        }
    }

    let selection = resolve(seeds, search_paths, libraries);
    // serde's `PathBuf` Serialize impl errors when a path component is not
    // valid UTF-8 (legal on Unix, possible on Windows via canonicalize edge
    // cases). Treat that as a cache write miss — degraded performance is
    // strictly better than poisoning the build with a panic.
    match bincode::serialize(&selection) {
        Ok(bytes) => {
            store.put(NAMESPACE, &key, &bytes)?;
        }
        Err(err) => {
            tracing::warn!(
                key = %key.to_hex(),
                error = %err,
                "library-select cache: failed to serialize selection \
                 (likely non-UTF-8 path); skipping cache write"
            );
        }
    }

    Ok(CachedSelection {
        selection,
        key,
        from_cache: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn write(path: &Path, contents: &str) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, contents).unwrap();
    }

    fn lib(tmp: &Path, name: &str) -> FrameworkLibrary {
        let dir = tmp.join("libraries").join(name);
        let src = dir.join("src");
        std::fs::create_dir_all(&src).unwrap();
        FrameworkLibrary {
            name: name.to_string(),
            dir,
            include_dirs: vec![src],
            source_files: Vec::new(),
        }
    }

    fn fixture_inputs<'a>(framework_root: &'a Path) -> CacheKeyInputs<'a> {
        CacheKeyInputs {
            toolchain_triple: "avr-unknown-none",
            framework_install_path: framework_root,
            framework_version: "1.59.0",
        }
    }

    fn build_simple_project() -> (TempDir, Vec<PathBuf>, Vec<PathBuf>, Vec<FrameworkLibrary>) {
        let tmp = TempDir::new().unwrap();
        let project_src = tmp.path().join("project").join("src");
        write(&project_src.join("main.cpp"), "#include <SPI.h>\n");

        let mut spi = lib(tmp.path(), "SPI");
        write(
            &spi.include_dirs[0].join("SPI.h"),
            "// canonical SPI header",
        );
        let spi_cpp = spi.include_dirs[0].join("SPI.cpp");
        write(&spi_cpp, "");
        spi.source_files.push(spi_cpp);

        let seeds = vec![project_src.join("main.cpp")];
        let search_paths = vec![project_src];
        (tmp, seeds, search_paths, vec![spi])
    }

    #[test]
    fn c01_cache_miss_then_hit() {
        let (tmp, seeds, search_paths, libs) = build_simple_project();
        let kv = FileKvStore::open(tmp.path().join("kv")).unwrap();
        let inputs = fixture_inputs(tmp.path());

        let first = resolve_cached(&seeds, &search_paths, &libs, &inputs, &kv).unwrap();
        assert!(!first.from_cache, "first call should miss");
        assert_eq!(first.selection.required_libraries, vec!["SPI".to_string()]);

        let second = resolve_cached(&seeds, &search_paths, &libs, &inputs, &kv).unwrap();
        assert!(second.from_cache, "second call should hit");
        assert_eq!(first.selection, second.selection);
        assert_eq!(first.key.as_bytes(), second.key.as_bytes());
    }

    #[test]
    fn c02_seed_content_change_invalidates_key() {
        let (tmp, seeds, search_paths, libs) = build_simple_project();
        let inputs = fixture_inputs(tmp.path());
        let key_before = cache_key(&seeds, &search_paths, &libs, &inputs);

        // Touch the seed body. Even though the include set is identical,
        // the cache key must change so a downstream "did the source actually
        // change" check is the source of truth, not the cached resolver
        // result. (Otherwise a syntactic-only edit could mask a real change.)
        write(&seeds[0], "#include <SPI.h>\n// extra comment\n");
        let key_after = cache_key(&seeds, &search_paths, &libs, &inputs);
        assert_ne!(key_before.as_bytes(), key_after.as_bytes());
    }

    #[test]
    fn c03_toolchain_change_invalidates_key() {
        let (tmp, seeds, search_paths, libs) = build_simple_project();
        let a = CacheKeyInputs {
            toolchain_triple: "avr-unknown-none",
            framework_install_path: tmp.path(),
            framework_version: "1.59.0",
        };
        let b = CacheKeyInputs {
            toolchain_triple: "xtensa-esp32-elf",
            framework_install_path: tmp.path(),
            framework_version: "1.59.0",
        };
        assert_ne!(
            cache_key(&seeds, &search_paths, &libs, &a).as_bytes(),
            cache_key(&seeds, &search_paths, &libs, &b).as_bytes()
        );
    }

    #[test]
    fn c04_framework_version_change_invalidates_key() {
        let (tmp, seeds, search_paths, libs) = build_simple_project();
        let a = fixture_inputs(tmp.path());
        let b = CacheKeyInputs {
            toolchain_triple: a.toolchain_triple,
            framework_install_path: a.framework_install_path,
            framework_version: "1.60.0",
        };
        assert_ne!(
            cache_key(&seeds, &search_paths, &libs, &a).as_bytes(),
            cache_key(&seeds, &search_paths, &libs, &b).as_bytes()
        );
    }

    #[test]
    fn c05_library_input_order_does_not_affect_key() {
        let tmp = TempDir::new().unwrap();
        let project_src = tmp.path().join("project").join("src");
        write(&project_src.join("main.cpp"), "#include <SPI.h>\n");

        let mut spi = lib(tmp.path(), "SPI");
        write(&spi.include_dirs[0].join("SPI.h"), "spi");
        spi.source_files.push(spi.include_dirs[0].join("SPI.cpp"));
        write(&spi.source_files[0], "");

        let mut wire = lib(tmp.path(), "Wire");
        write(&wire.include_dirs[0].join("Wire.h"), "wire");
        wire.source_files
            .push(wire.include_dirs[0].join("Wire.cpp"));
        write(&wire.source_files[0], "");

        let seeds = vec![project_src.join("main.cpp")];
        let search_paths = vec![project_src];
        let inputs = fixture_inputs(tmp.path());

        let k1 = cache_key(&seeds, &search_paths, &[spi.clone(), wire.clone()], &inputs);
        let k2 = cache_key(&seeds, &search_paths, &[wire, spi], &inputs);
        assert_eq!(
            k1.as_bytes(),
            k2.as_bytes(),
            "library input order must be normalized"
        );
    }

    #[test]
    fn c06_search_path_order_does_affect_key() {
        // PIO resolves earlier search paths first, so the key must reflect
        // ordering. (If this test fails after a refactor, it's because
        // `cache_key` accidentally sorted search_paths — don't.)
        let (tmp, seeds, search_paths, libs) = build_simple_project();
        let inputs = fixture_inputs(tmp.path());

        let mut reversed = search_paths.clone();
        reversed.push(tmp.path().join("extra"));
        let mut also_reversed = reversed.clone();
        also_reversed.reverse();

        assert_ne!(
            cache_key(&seeds, &reversed, &libs, &inputs).as_bytes(),
            cache_key(&seeds, &also_reversed, &libs, &inputs).as_bytes()
        );
    }

    #[test]
    fn c07_corrupt_entry_falls_through_to_recompute() {
        let (tmp, seeds, search_paths, libs) = build_simple_project();
        let kv = FileKvStore::open(tmp.path().join("kv")).unwrap();
        let inputs = fixture_inputs(tmp.path());

        let key = cache_key(&seeds, &search_paths, &libs, &inputs);
        // Plant garbage bytes under the cache key. resolve_cached must NOT
        // propagate the bincode error — it must fall through, recompute,
        // and overwrite.
        kv.put(NAMESPACE, &key, b"not a valid bincoded Selection")
            .unwrap();

        let res = resolve_cached(&seeds, &search_paths, &libs, &inputs, &kv).unwrap();
        assert!(!res.from_cache, "corrupt entry must trigger recompute");
        assert_eq!(res.selection.required_libraries, vec!["SPI".to_string()]);

        // And the corrupted bytes were replaced with valid bincode.
        let again = resolve_cached(&seeds, &search_paths, &libs, &inputs, &kv).unwrap();
        assert!(again.from_cache);
    }
}
