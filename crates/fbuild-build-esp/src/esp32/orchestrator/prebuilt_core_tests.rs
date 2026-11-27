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
