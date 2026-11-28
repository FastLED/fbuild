//! Consumer fixtures for the prebuilt core-object ladder (#1882, #2196 cell 3):
//! a real HTTP server, real tar.zst bytes, and the real hydration rule.
use super::*;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::io::Write;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use tar::{Builder, Header};

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// One tar.zst holding `entries` (name, body) at mode 0644.
fn tar_zst(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let encoder = zstd::Encoder::new(Vec::new(), 3).unwrap();
    let mut builder = Builder::new(encoder);
    for (name, body) in entries {
        let mut header = Header::new_gnu();
        header.set_size(body.len() as u64);
        header.set_mode(0o644);
        header.set_entry_type(tar::EntryType::Regular);
        header.set_cksum();
        builder.append_data(&mut header, name, *body).unwrap();
    }
    let mut encoder = builder.into_inner().unwrap();
    encoder.flush().unwrap();
    encoder.finish().unwrap()
}

/// Bind a loopback listener and return its base URL, so bodies that must name
/// absolute URLs can be built before the server starts.
async fn bind() -> (String, tokio::net::TcpListener) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = format!("http://{}", listener.local_addr().unwrap());
    (address, listener)
}

/// Serve `routes` (exact path -> body) on `listener`, 404 for anything else,
/// counting every request so a test can prove what was fetched.
fn spawn_paths(
    listener: tokio::net::TcpListener,
    routes: BTreeMap<String, Vec<u8>>,
) -> (Arc<AtomicUsize>, tokio::task::JoinHandle<()>) {
    let count = Arc::new(AtomicUsize::new(0));
    let calls = count.clone();
    let routes = Arc::new(routes);
    let app = axum::Router::new().fallback(move |request: axum::extract::Request| {
        let routes = routes.clone();
        let calls = calls.clone();
        async move {
            calls.fetch_add(1, Ordering::SeqCst);
            let path = request.uri().path().trim_start_matches('/').to_string();
            match routes.get(&path) {
                Some(body) => (axum::http::StatusCode::OK, body.clone()),
                None => (axum::http::StatusCode::NOT_FOUND, Vec::new()),
            }
        }
    });
    let handle = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (count, handle)
}

/// A framework-shaped source tree plus the canonical unit ids of its sources.
fn framework_units(root: &Path) -> Vec<(String, NormalizedPath)> {
    let dirs = [
        ("cores/esp32", NormalizedPath::new(root).join("cores/esp32")),
        (
            "variants/esp32c3",
            NormalizedPath::new(root).join("variants/esp32c3"),
        ),
    ];
    let mut units = Vec::new();
    for (id, dir) in dirs {
        std::fs::create_dir_all(dir.as_path()).unwrap();
        let source = dir.join("unit.cpp");
        std::fs::write(source.as_path(), "// unit\n").unwrap();
        units.push((format!("{id}/unit.cpp"), source));
    }
    units
}

/// A served tree hydrates to the object paths this host's compiler expects, so
/// the ordinary incremental check then compiles nothing for those units.
#[test]
fn a_served_tree_hydrates_every_unit_it_covers() {
    let temp = tempfile::tempdir().unwrap();
    let units = framework_units(temp.path());
    let build_dir = temp.path().join("core-build");
    std::fs::create_dir_all(&build_dir).unwrap();

    // A publisher's tree: one object per unit, named by the manifest, plus an
    // entry this build does not ask for (it must be ignored).
    let tree = temp.path().join("tree");
    std::fs::create_dir_all(&tree).unwrap();
    let manifest = serde_json::json!({
        "schema": 1,
        "units": {
            units[0].0.clone(): "unit_ab.cpp.o",
            units[1].0.clone(): "variant_cd.cpp.o",
            "cores/esp32/extra.cpp": "extra_ef.cpp.o",
        }
    });
    std::fs::write(tree.join(OBJECTS_MANIFEST), manifest.to_string()).unwrap();
    for name in ["unit_ab.cpp.o", "variant_cd.cpp.o", "extra_ef.cpp.o"] {
        std::fs::write(tree.join(name), name).unwrap();
    }

    let hydrated = hydrate(&tree, &build_dir, &units).unwrap();
    assert_eq!(hydrated, 2, "only this build's units are hydrated");
    assert!(
        !build_dir.join("extra_ef.cpp.o").exists(),
        "no stray writes"
    );

    for (_, source) in &units {
        let object = crate::compiler::CompilerBase::object_path(source.as_path(), &build_dir);
        assert!(object.is_file(), "{} was not hydrated", object.display());
        assert!(
            !crate::compiler::CompilerBase::needs_rebuild(source.as_path(), &object),
            "a hydrated object must satisfy the incremental check"
        );
    }
}

/// The publisher's archive is the client's input, end to end: a tree packed by
/// `prebuilt::pack` carries the `objects.json` this module reads, so an artifact
/// the builder can write is one the client can hydrate — proven through the real
/// extractor, not by a hand-written manifest.
#[test]
fn a_packed_tree_hydrates_through_the_real_publisher() {
    use fbuild_packages_fetch::prebuilt::{
        Artifact, DEFAULT_PRODUCT, PackRequest, UnitRef, extract, pack,
    };

    let temp = tempfile::tempdir().unwrap();
    let units = framework_units(temp.path());
    let build_dir = temp.path().join("core-build");
    std::fs::create_dir_all(&build_dir).unwrap();

    // The publisher's build directory holds one object per unit, named the way
    // its own compiler derives it — the names are not portable, so what the
    // archive must carry is the unit-id relation.
    let publisher = temp.path().join("publisher-build");
    std::fs::create_dir_all(&publisher).unwrap();
    let mut names = Vec::new();
    for (_, source) in &units {
        let object = crate::compiler::CompilerBase::object_path(source.as_path(), &publisher);
        std::fs::write(&object, b"published object").unwrap();
        names.push(object.file_name().unwrap().to_string_lossy().into_owned());
    }
    std::fs::write(publisher.join("README.txt"), b"payload").unwrap();
    let refs: Vec<UnitRef<'_>> = units
        .iter()
        .zip(&names)
        .map(|((id, _), name)| UnitRef {
            unit: id,
            object: name,
        })
        .collect();

    let out = temp.path().join("out");
    let fingerprint = "0123456789abcdef".repeat(4);
    let packed = pack(&PackRequest {
        objects: &publisher,
        out: &out,
        vendor: "espressif",
        product: DEFAULT_PRODUCT,
        channel: "release",
        reference: "3.3.12",
        chip: "esp32c3",
        framework: "arduino-esp32",
        fingerprint: &fingerprint,
        units: &refs,
        arch: "esp32c3",
        abi: "gcc@14.2.0",
        base_url: "https://bucket.example",
        min_client_version: 1,
    })
    .unwrap();
    assert_eq!(packed.units, 2);

    let artifact = Artifact {
        path: packed.archive.clone(),
        size_bytes: packed.size_bytes,
        sha256: packed.sha256.clone(),
    };
    let tree = temp.path().join("served");
    let extracted = extract(&artifact, &tree).unwrap();
    assert_eq!(extracted.root.as_path(), tree.as_path());

    let hydrated = hydrate(&tree, &build_dir, &units).unwrap();
    assert_eq!(hydrated, 2, "both published units hydrated");
    for (_, source) in &units {
        let object = crate::compiler::CompilerBase::object_path(source.as_path(), &build_dir);
        assert_eq!(
            std::fs::read(&object).unwrap(),
            b"published object",
            "the published bytes landed at this host's own object path"
        );
    }
}

/// A tree without the manifest is refused: an archive must never be able to
/// name an object the compiler did not ask for.
#[test]
fn a_tree_without_a_manifest_is_refused() {
    let temp = tempfile::tempdir().unwrap();
    let units = framework_units(temp.path());
    let build_dir = temp.path().join("core-build");
    std::fs::create_dir_all(&build_dir).unwrap();
    let error = hydrate(temp.path(), &build_dir, &units).unwrap_err();
    assert!(error.contains(OBJECTS_MANIFEST), "{error}");
}

/// A manifest from a newer publisher is refused rather than guessed at.
#[test]
fn a_manifest_from_a_newer_publisher_is_refused() {
    let temp = tempfile::tempdir().unwrap();
    let units = framework_units(temp.path());
    let build_dir = temp.path().join("core-build");
    std::fs::create_dir_all(&build_dir).unwrap();
    let manifest = serde_json::json!({ "schema": 2, "units": {} });
    std::fs::write(temp.path().join(OBJECTS_MANIFEST), manifest.to_string()).unwrap();
    let error = hydrate(temp.path(), &build_dir, &units).unwrap_err();
    assert!(error.contains("schema 2"), "{error}");
}

/// A unit the tree does not cover is left to the compiler; a tree that covers
/// none of them is not served at all.
#[test]
fn a_tree_that_covers_no_unit_is_reported_as_unusable() {
    let temp = tempfile::tempdir().unwrap();
    let units = framework_units(temp.path());
    let build_dir = temp.path().join("core-build");
    std::fs::create_dir_all(&build_dir).unwrap();
    let manifest = serde_json::json!({ "schema": 1, "units": { "cores/esp32/other.cpp": "x.o" } });
    std::fs::write(temp.path().join(OBJECTS_MANIFEST), manifest.to_string()).unwrap();
    assert_eq!(hydrate(temp.path(), &build_dir, &units).unwrap(), 0);
}

/// The canonical unit id is the source's path relative to the framework root, so
/// two hosts that installed the framework elsewhere agree on it.
#[test]
fn unit_ids_are_framework_relative_and_separator_stable() {
    let temp = tempfile::tempdir().unwrap();
    let units = framework_units(temp.path());
    let root = NormalizedPath::new(temp.path());
    assert_eq!(
        units[0].0,
        units[0].1.relative_to(&root).unwrap().display_slash()
    );
    assert!(units[0].0.starts_with("cores/esp32/"));
}

/// End to end through the catalog and the ladder: the client's exact query
/// resolves to a published entry, the archive is fetched, and the tree hydrates.
#[tokio::test]
async fn the_ladder_serves_a_published_tree_and_it_is_hydrated() {
    let temp = tempfile::tempdir().unwrap();
    let units = framework_units(temp.path());
    let build_dir = temp.path().join("core-build");
    std::fs::create_dir_all(&build_dir).unwrap();
    let fingerprint = "a".repeat(64);
    let manifest = serde_json::json!({
        "schema": 1,
        "units": {
            units[0].0.clone(): "unit_ab.cpp.o",
            units[1].0.clone(): "variant_cd.cpp.o",
        }
    })
    .to_string();
    let archive = tar_zst(&[
        (OBJECTS_MANIFEST, manifest.as_bytes()),
        ("unit_ab.cpp.o", b"object"),
        ("variant_cd.cpp.o", b"object"),
    ]);

    // Bind first: the shard must name the archive by absolute URL.
    let (base, listener) = bind().await;
    let tool = format!("prebuilt-{FRAMEWORK}");
    let shard = serde_json::json!({
        "kind": "Catalog",
        "schema_version": 1,
        "tool": tool,
        "channels": { "3.3.12": "3.3.12" },
        "releases": [{
            "tool": tool,
            "version": "3.3.12",
            "min_client_version": 1,
            "platforms": [{
                "platform": { "os": "none", "arch": "esp32c3", "abi": "esp32c3" },
                "variant": { "flavor": format!("fp-{}", &fingerprint[..16]) },
                "asset": {
                    "filename": "prebuilt-esp32c3.tar.zst",
                    "size_bytes": archive.len(),
                    "sha256": digest(&archive),
                    "urls": [format!("{base}/archive.tar.zst")],
                    "provides": [format!("fbuild-fp:{fingerprint}")],
                    "parts": [],
                }
            }]
        }]
    })
    .to_string();
    let index = serde_json::json!({
        "kind": "Index",
        "schema_version": 1,
        "tools": { tool.clone(): { "descriptor": {
            "url": format!("{tool}.json"),
            "sha256": digest(shard.as_bytes()),
            "size_bytes": shard.len(),
        } } }
    })
    .to_string();
    let routes: BTreeMap<String, Vec<u8>> = [
        ("manifest.json".to_string(), index.into_bytes()),
        (format!("{tool}.json"), shard.into_bytes()),
        ("archive.tar.zst".to_string(), archive),
    ]
    .into_iter()
    .collect();
    let (calls, task) = spawn_paths(listener, routes);

    let cache = temp.path().join("cache");
    let reader = Reader::new(
        &cache,
        vec![format!("{base}/manifest.json")],
        reqwest::Client::new(),
    );
    let query = Query {
        framework: FRAMEWORK.to_string(),
        arch: "esp32c3".to_string(),
        abi: "esp32c3".to_string(),
        version: "3.3.12".to_string(),
        fp: fingerprint.clone(),
    };
    let Resolution::Hit(asset) = reader.resolve(&query).await else {
        panic!("the published catalog entry must resolve");
    };
    let fetcher = Fetcher::new(&temp.path().join("archives"), reqwest::Client::new());
    let served = serve(&cache, &query, &asset, &fetcher).await;
    assert!(
        calls.load(Ordering::SeqCst) >= 3,
        "index + shard + archive fetched"
    );
    let Served::Prebuilt(obtained) = served else {
        panic!("expected a served tree");
    };
    let hydrated = hydrate(obtained.tree.root.as_path(), &build_dir, &units).unwrap();
    assert_eq!(hydrated, 2);
    for (_, source) in &units {
        let object = crate::compiler::CompilerBase::object_path(source.as_path(), &build_dir);
        assert!(object.is_file(), "{} was not hydrated", object.display());
        assert!(!crate::compiler::CompilerBase::needs_rebuild(
            source.as_path(),
            &object
        ));
    }
    task.abort();
}

/// #2196 cell 4: the record `build_info.json` carries. A served tree names the
/// artifact it linked; a source build says `used: false` and keeps the ladder's
/// reason, so the `#1882` benchmark can tell its two arms apart from the file.
#[test]
fn a_served_consult_records_the_artifact_it_hydrated() {
    let outcome = Outcome {
        notice: None,
        hydrated: 2,
        fingerprint: Some("a".repeat(64)),
        served_from: Some("https://example.invalid/v1/core.tar.zst".to_string()),
        key: Some("arduino-esp32/3.3.12/esp32c3/fp-0123456789abcdef".to_string()),
        sha256: Some("b".repeat(64)),
    };
    let record = outcome.record();
    assert!(record.used);
    assert_eq!(record.source, "https://example.invalid/v1/core.tar.zst");
    assert_eq!(
        record.key.as_deref(),
        Some("arduino-esp32/3.3.12/esp32c3/fp-0123456789abcdef")
    );
    assert_eq!(record.sha256.as_deref(), Some("b".repeat(64).as_str()));
    assert_eq!(record.reason, None);
}

#[test]
fn a_source_build_records_used_false_and_why() {
    let outcome = Outcome::source(
        "prebuilt core: no published tree for this identity (miss)".to_string(),
        Some("c".repeat(64)),
    );
    let record = outcome.record();
    assert!(!record.used);
    assert_eq!(record.source, "source");
    assert_eq!(record.key, None);
    assert_eq!(record.sha256, None);
    assert!(
        record
            .reason
            .as_deref()
            .is_some_and(|reason| reason.contains("no published tree")),
        "a declined consult keeps its reason: {record:?}"
    );
}

#[test]
fn a_tree_that_covers_nothing_is_a_source_build() {
    let outcome = Outcome::source("prebuilt core: covers none".to_string(), None);
    assert!(!outcome.record().used);
}

/// The key the record names is the identity the publisher uploads under — the
/// same `{framework}/{version}/{arch}/fp-<sha16>` relative path
/// `prebuilt::tree_root` extracts into.
#[test]
fn the_recorded_key_is_the_published_tree_path() {
    let fingerprint = "0123456789abcdef".repeat(4);
    let query = Query {
        framework: FRAMEWORK.to_string(),
        arch: "esp32c3".to_string(),
        abi: "esp32c3".to_string(),
        version: "3.3.12".to_string(),
        fp: fingerprint.clone(),
    };
    assert_eq!(
        identity_key(&query),
        format!("arduino-esp32/3.3.12/esp32c3/fp-{}", &fingerprint[..16])
    );
}

/// Archive one arm's objects exactly the way the library stage does: sorted
/// member list, the shared `ar` primitive, the 60 s library watchdog (#844).
async fn archive(ar: &str, objects: &[NormalizedPath], output: &Path) {
    fbuild_core::archive::create(
        Path::new(ar),
        objects,
        output,
        "ar",
        std::time::Duration::from_secs(60),
    )
    .await
    .unwrap();
}

/// One member of a GNU archive, header fields included: two deterministic
/// archives of the same objects must agree on all of them, not just on the
/// payload bytes.
#[derive(Debug, PartialEq, Eq)]
struct Member {
    name: String,
    mode: String,
    uid: String,
    gid: String,
    mtime: String,
    body: Vec<u8>,
}

/// Parse a GNU archive's members. The symbol index (`/`) is skipped: it is
/// derived from the members, so comparing it would only restate them.
fn members(archive: &[u8]) -> Vec<Member> {
    let text = |range: std::ops::Range<usize>| {
        String::from_utf8_lossy(archive.get(range).unwrap_or_default())
            .trim()
            .to_string()
    };
    let mut members = Vec::new();
    // Skip the 8-byte `!<arch>\n` global header; members follow it.
    let mut offset = 8;
    while offset + 60 <= archive.len() {
        if archive.get(offset + 58..offset + 60) != Some(b"`\n".as_slice()) {
            break;
        }
        let name = text(offset..offset + 16).trim_end_matches('/').to_string();
        let size: usize = text(offset + 48..offset + 58).parse().unwrap_or(0);
        let body = offset + 60;
        if body + size > archive.len() {
            break;
        }
        if name != "/" && !name.is_empty() {
            members.push(Member {
                name,
                mode: text(offset + 40..offset + 48),
                uid: text(offset + 28..offset + 34),
                gid: text(offset + 34..offset + 40),
                mtime: text(offset + 16..offset + 28),
                body: archive[body..body + size].to_vec(),
            });
        }
        offset = body + size + (size & 1);
    }
    members
}

/// A digest tells you the two arms disagree; the member table says where.
/// Compare headers and payloads first, so a served-vs-source regression names
/// the unit whose object changed, and a header-only difference names the field
/// the archiver stamped (which is what `ar Drcs` exists to prevent).
fn assert_same_archive(fresh: &[u8], reused: &[u8]) {
    assert_eq!(
        members(fresh),
        members(reused),
        "the served tree's members differ from the from-source build's"
    );
    assert_eq!(
        digest(fresh),
        digest(reused),
        "every member and header matches, so the bytes differ outside them: {} vs {} bytes; \
         prefix {:?} vs {:?}",
        fresh.len(),
        reused.len(),
        String::from_utf8_lossy(fresh.get(..72).unwrap_or_default()),
        String::from_utf8_lossy(reused.get(..72).unwrap_or_default())
    );
    assert_eq!(
        fresh, reused,
        "the framework archive must be byte-identical"
    );
}

/// The **framework archive** is what a build links, so the served and the
/// from-source arm must agree on its bytes — not merely on the entry names
/// (#2192 rule: assert `reused == fresh`). Hydration copies the publisher's
/// object verbatim to this host's own object path and both arms hand the
/// archiver the same member list in the same order, so a deterministic `ar`
/// must write the same archive from two different build directories.
#[tokio::test]
async fn a_served_tree_and_a_source_build_archive_to_identical_bytes() {
    use fbuild_packages_fetch::prebuilt::{
        Artifact, DEFAULT_PRODUCT, PackRequest, UnitRef, extract, pack,
    };

    let temp = tempfile::tempdir().unwrap();
    let units = framework_units(temp.path());
    let ar = std::env::var("FBUILD_TEST_AR").unwrap_or_else(|_| "ar".into());

    // The from-source arm: the compiler wrote one object per unit into the
    // project's core build dir, under the names this host derives.
    let fresh_dir = temp.path().join("fresh-core-build");
    std::fs::create_dir_all(&fresh_dir).unwrap();
    // Pair each unit with the object this host derives for it and sort the
    // PAIRS. Sorting the paths alone and then zipping them against `units`
    // maps one unit to another unit's object whenever the hash order differs
    // from the unit order (#2203: the served arm then archived swapped bytes).
    let mut fresh_pairs: Vec<(String, NormalizedPath)> = units
        .iter()
        .enumerate()
        .map(|(index, (unit, source))| {
            let object = crate::compiler::CompilerBase::object_path(source.as_path(), &fresh_dir);
            std::fs::write(&object, format!("compiled object {index}")).unwrap();
            (unit.clone(), NormalizedPath::new(object))
        })
        .collect();
    fresh_pairs.sort_by(|left, right| left.1.cmp(&right.1));
    let fresh_objects: Vec<NormalizedPath> = fresh_pairs
        .iter()
        .map(|(_, object)| NormalizedPath::new(object.as_path()))
        .collect();
    let fresh_archive = temp.path().join("fresh").join("libcore.a");
    archive(&ar, &fresh_objects, fresh_archive.as_path()).await;
    let fresh = std::fs::read(&fresh_archive).unwrap();
    assert!(!fresh.is_empty(), "the from-source arm produced no archive");

    // The publisher packs exactly those bytes; the client extracts the archive,
    // hydrates it into a DIFFERENT build dir, and archives what it received.
    let names: Vec<String> = fresh_pairs
        .iter()
        .map(|(_, object)| {
            object
                .as_path()
                .file_name()
                .unwrap()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    let refs: Vec<UnitRef<'_>> = fresh_pairs
        .iter()
        .zip(&names)
        .map(|((unit, _), name)| UnitRef {
            unit: unit.as_str(),
            object: name.as_str(),
        })
        .collect();
    let fingerprint = "ab".repeat(32);
    let packed = pack(&PackRequest {
        objects: fresh_dir.as_path(),
        out: temp.path().join("out").as_path(),
        vendor: "espressif",
        product: DEFAULT_PRODUCT,
        channel: "release",
        reference: "3.3.12",
        chip: "esp32c3",
        framework: "arduino-esp32",
        fingerprint: &fingerprint,
        units: &refs,
        arch: "esp32c3",
        abi: "gcc@14.2.0",
        base_url: "https://bucket.example",
        min_client_version: 1,
    })
    .unwrap();

    let tree = temp.path().join("served-tree");
    extract(
        &Artifact {
            path: packed.archive.clone(),
            size_bytes: packed.size_bytes,
            sha256: packed.sha256.clone(),
        },
        &tree,
    )
    .unwrap();
    let reused_dir = temp.path().join("reused-core-build");
    std::fs::create_dir_all(&reused_dir).unwrap();
    assert_eq!(
        hydrate(&tree, &reused_dir, &units).unwrap(),
        units.len(),
        "the served tree covers every unit of this cell"
    );

    let mut reused_objects = Vec::new();
    for (_, source) in &units {
        let object = crate::compiler::CompilerBase::object_path(source.as_path(), &reused_dir);
        reused_objects.push(NormalizedPath::new(object));
    }
    reused_objects.sort();
    let reused_archive = temp.path().join("reused").join("libcore.a");
    archive(&ar, &reused_objects, reused_archive.as_path()).await;
    let reused = std::fs::read(&reused_archive).unwrap();

    assert_ne!(
        fresh_dir.as_path(),
        reused_dir.as_path(),
        "the two arms must not archive the same directory"
    );
    assert_same_archive(&fresh, &reused);
}
