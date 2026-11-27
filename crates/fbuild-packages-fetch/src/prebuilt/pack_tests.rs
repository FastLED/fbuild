//! Publisher fixtures for the artifact builder (#1882): a real objects tree is
//! packed, then served from a real HTTP server and resolved by the real reader
//! and extractor, so the archive and the entry agree by construction.
use super::*;
use crate::prebuilt::{Fetcher, Query, Reader, Resolution, obtain};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::path::Path;
use tempfile::TempDir;

const FP: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

/// The two compiled units [`objects`] holds, addressed the way the client does.
const UNITS: [UnitRef<'static>; 2] = [
    UnitRef {
        unit: "cores/esp32/main.c",
        object: "cores/esp32/main.c.o",
    },
    UnitRef {
        unit: "cores/esp32/hal.c",
        object: "cores/esp32/hal.c.o",
    },
];

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// A small objects tree: a nested directory, an ordinary file, and an
/// executable one so the mode path is exercised.
fn objects() -> (TempDir, NormalizedPath) {
    let dir = TempDir::new().unwrap();
    let root = NormalizedPath::new(dir.path().join("objects"));
    std::fs::create_dir_all(root.join("cores/esp32").as_path()).unwrap();
    std::fs::write(root.join("cores/esp32/main.c.o").as_path(), b"main object").unwrap();
    std::fs::write(root.join("cores/esp32/hal.c.o").as_path(), b"hal object").unwrap();
    std::fs::write(root.join("README.txt").as_path(), b"packed by fbuild").unwrap();
    let tool = root.join("tool");
    std::fs::write(tool.as_path(), b"#!/bin/sh\n").unwrap();
    fbuild_core::platform::fs::set_executable(tool.as_path()).unwrap();
    (dir, root)
}

fn request<'a>(objects: &'a Path, out: &'a Path, base_url: &'a str) -> PackRequest<'a> {
    PackRequest {
        objects,
        out,
        vendor: "espressif",
        product: DEFAULT_PRODUCT,
        channel: "release",
        reference: "3.3.12",
        chip: "esp32dev",
        framework: "arduino-esp32",
        fingerprint: FP,
        units: &UNITS,
        arch: "esp32",
        abi: "gcc@14.2.0",
        base_url,
        min_client_version: 1,
    }
}

#[test]
fn the_key_is_derived_from_product_reference_chip_and_digest() {
    let (dir, root) = objects();
    let out = dir.path().join("out");
    let packed = pack(&request(&root, &out, "https://bucket.example")).unwrap();
    let expected = format!(
        "v1/espressif/{DEFAULT_PRODUCT}/release/3.3.12/esp32dev/\
         {DEFAULT_PRODUCT}-3.3.12-esp32dev.{}.tar.zst",
        &packed.sha256[..16]
    );
    assert_eq!(packed.key, expected);
    assert_eq!(packed.filename(), expected.rsplit('/').next().unwrap());
    assert_eq!(packed.entries, 7, "six tree entries plus the unit manifest");
    assert_eq!(packed.units, 2);
    assert_eq!(
        std::fs::read(packed.archive.as_path()).unwrap().len() as u64,
        packed.size_bytes
    );
    assert_eq!(
        packed.urls("https://bucket.example/"),
        vec![format!("https://bucket.example/{expected}")]
    );
}

#[test]
fn packing_is_deterministic_across_mtimes_and_output_directories() {
    let (dir, root) = objects();
    let first = pack(&request(
        &root,
        &dir.path().join("a"),
        "https://bucket.example",
    ))
    .unwrap();
    // Move every mtime forward: a publisher's clock must not change the digest.
    std::fs::File::options()
        .write(true)
        .open(root.join("README.txt").as_path())
        .unwrap()
        .set_modified(std::time::SystemTime::now() + std::time::Duration::from_secs(9_000))
        .unwrap();
    let second = pack(&request(
        &root,
        &dir.path().join("b"),
        "https://bucket.example",
    ))
    .unwrap();
    assert_eq!(first.sha256, second.sha256);
    assert_eq!(first.size_bytes, second.size_bytes);
    assert_eq!(first.key, second.key);
}

#[test]
fn the_archive_round_trips_through_resolve_fetch_and_extract() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async {
        let (dir, root) = objects();
        let out = dir.path().join("out");
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        let packed = pack(&request(&root, &out, &address)).unwrap();
        let archive = std::fs::read(packed.archive.as_path()).unwrap();
        let shard = serde_json::to_vec(&catalog(
            &tool_for("arduino-esp32"),
            vec![release(&request(&root, &out, &address), &packed)],
        ))
        .unwrap();
        let index = serde_json::to_vec(&json!({
            "kind": "Index",
            "schema_version": 1,
            "tools": {
                "prebuilt-arduino-esp32": {
                    "descriptor": {
                        "url": "shards/prebuilt-arduino-esp32.json",
                        "sha256": digest(&shard),
                        "size_bytes": shard.len(),
                    }
                }
            }
        }))
        .unwrap();
        let served_index = index.clone();
        let served_shard = shard.clone();
        let served_archive = archive.clone();
        let app = axum::Router::new().fallback(move |request: axum::extract::Request| {
            let path = request.uri().path().to_string();
            let index = served_index.clone();
            let shard = served_shard.clone();
            let archive = served_archive.clone();
            async move {
                let body = if path.ends_with("manifest.json") {
                    index
                } else if path.contains("/shards/") {
                    shard
                } else {
                    archive
                };
                (axum::http::StatusCode::OK, body)
            }
        });
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let query = Query {
            framework: "arduino-esp32".into(),
            arch: "esp32".into(),
            abi: "gcc@14.2.0".into(),
            version: "3.3.12".into(),
            fp: FP.into(),
        };
        let reader = Reader::new(
            dir.path().join("cache").as_path(),
            vec![format!("{address}/mirror/manifest.json")],
            crate::http::client_with_timeout(std::time::Duration::from_secs(10)),
        );
        let asset = match reader.resolve(&query).await {
            Resolution::Hit(asset) => asset,
            Resolution::Miss(reason) => panic!("entry was not servable: {reason}"),
        };
        assert_eq!(asset.sha256, packed.sha256);
        assert_eq!(asset.size_bytes, packed.size_bytes);
        assert_eq!(asset.provides, vec![format!("fbuild-fp:{FP}")]);

        let cache = dir.path().join("trees");
        let fetcher = Fetcher::new(
            dir.path().join("downloads").as_path(),
            crate::http::client_with_timeout(std::time::Duration::from_secs(10)),
        );
        let obtained = obtain(cache.as_path(), &query, &asset, &fetcher)
            .await
            .unwrap();
        let tree = obtained.tree.root;
        for relative in ["cores/esp32/main.c.o", "cores/esp32/hal.c.o", "README.txt"] {
            assert_eq!(
                std::fs::read(tree.join(relative).as_path()).unwrap(),
                std::fs::read(root.join(relative).as_path()).unwrap(),
                "{relative} survived the round trip"
            );
        }
        assert!(
            fbuild_core::platform::fs::is_executable(
                &std::fs::metadata(tree.join("tool").as_path()).unwrap()
            ),
            "the executable bit survives the round trip"
        );
        let manifest: serde_json::Value =
            serde_json::from_slice(&std::fs::read(tree.join(OBJECTS_MANIFEST).as_path()).unwrap())
                .unwrap();
        assert_eq!(manifest["schema"], MANIFEST_SCHEMA);
        assert_eq!(
            manifest["units"]["cores/esp32/main.c"],
            "cores/esp32/main.c.o"
        );
        assert_eq!(
            manifest["units"]["cores/esp32/hal.c"],
            "cores/esp32/hal.c.o"
        );
        server.abort();
    });
}

#[test]
fn a_tree_with_a_symlink_is_refused_before_anything_is_written() {
    let (dir, root) = objects();
    fbuild_core::platform::fs::symlink_file(
        root.join("README.txt").as_path(),
        root.join("link").as_path(),
    )
    .unwrap();
    let out = dir.path().join("out");
    let error = pack(&request(&root, &out, "https://bucket.example")).unwrap_err();
    assert!(matches!(error, PackError::EntryType(name, "symlink") if name == "link"));
    assert!(!out.exists(), "nothing was written");
}

#[test]
fn an_empty_tree_is_refused() {
    let dir = TempDir::new().unwrap();
    let root = NormalizedPath::new(dir.path().join("empty"));
    std::fs::create_dir_all(root.join("only/dirs").as_path()).unwrap();
    let error = pack(&request(
        &root,
        &dir.path().join("out"),
        "https://bucket.example",
    ))
    .unwrap_err();
    assert!(matches!(error, PackError::Empty));
}

#[test]
fn a_request_that_cannot_be_served_is_refused() {
    let (dir, root) = objects();
    let out = dir.path().join("out");
    let mut bad = request(&root, &out, "https://bucket.example");
    bad.fingerprint = "not-a-fingerprint";
    assert!(matches!(pack(&bad), Err(PackError::Invalid("fingerprint"))));

    let mut bad = request(&root, &out, "https://bucket.example");
    bad.channel = "latest";
    assert!(matches!(pack(&bad), Err(PackError::Invalid("channel"))));

    let bad = request(&root, &out, "ftp://bucket.example");
    assert!(pack(&bad).is_err(), "a non-https base URL is refused");

    let mut bad = request(&root, &out, "https://bucket.example");
    bad.chip = "../esp32";
    assert!(matches!(pack(&bad), Err(PackError::Invalid("chip"))));

    let mut bad = request(&root, &out, "https://bucket.example");
    bad.min_client_version = 0;
    assert!(matches!(
        pack(&bad),
        Err(PackError::Invalid("min_client_version"))
    ));
    assert!(!out.exists(), "a refused request writes nothing");
}

#[test]
fn a_request_pointing_at_a_missing_tree_is_refused() {
    let dir = TempDir::new().unwrap();
    let missing = NormalizedPath::new(dir.path().join("absent"));
    let error = pack(&request(
        &missing,
        &dir.path().join("out"),
        "https://bucket.example",
    ))
    .unwrap_err();
    assert!(matches!(error, PackError::Invalid("objects")));
}

#[test]
fn the_entry_names_the_flavor_and_the_fingerprint_the_client_queries_with() {
    let (dir, root) = objects();
    let out = dir.path().join("out");
    let packed = pack(&request(&root, &out, "https://bucket.example")).unwrap();
    let entry = release(&request(&root, &out, "https://bucket.example"), &packed);
    let json = serde_json::to_value(&entry).unwrap();
    let platform = &json["platforms"][0];
    assert_eq!(platform["variant"]["flavor"], format!("fp-{}", &FP[..16]));
    assert_eq!(platform["platform"]["os"], "none");
    assert_eq!(platform["asset"]["filename"], packed.filename());
    assert_eq!(platform["asset"]["sha256"], packed.sha256);
    assert_eq!(platform["asset"]["size_bytes"], packed.size_bytes);
    assert_eq!(json["tool"], "prebuilt-arduino-esp32");
    assert_eq!(json["version"], "3.3.12");
    assert_eq!(json["min_client_version"], 1);
}

/// A packed tree carries the unit manifest, and the digest is stable across two
/// packs of the same units in two output directories.
#[test]
fn the_archive_carries_the_unit_manifest() {
    let (dir, root) = objects();
    let out = dir.path().join("out");
    let packed = pack(&request(&root, &out, "https://bucket.example")).unwrap();
    let bytes = std::fs::read(packed.archive.as_path()).unwrap();
    let mut archive = tar::Archive::new(zstd::Decoder::new(&bytes[..]).unwrap());
    let manifest = archive
        .entries()
        .unwrap()
        .filter_map(Result::ok)
        .find(|entry| entry.path().unwrap().to_string_lossy() == OBJECTS_MANIFEST)
        .expect("the manifest is in the archive");
    let body: serde_json::Value = serde_json::from_reader(manifest).unwrap();
    assert_eq!(body["schema"], MANIFEST_SCHEMA);
    assert_eq!(body["units"].as_object().unwrap().len(), 2);
}

/// A unit that names an object the tree does not hold is refused before a byte
/// is written: a manifest may never advertise an unservable file.
#[test]
fn a_unit_naming_a_missing_object_is_refused() {
    let (dir, root) = objects();
    let out = dir.path().join("out");
    let missing = [UnitRef {
        unit: "cores/esp32/gone.c",
        object: "cores/esp32/gone.c.o",
    }];
    let mut bad = request(&root, &out, "https://bucket.example");
    bad.units = &missing;
    let error = pack(&bad).unwrap_err();
    assert!(
        matches!(error, PackError::MissingObject(ref unit, ref object)
            if unit == "cores/esp32/gone.c" && object == "cores/esp32/gone.c.o"),
        "{error}"
    );
    assert!(!out.exists(), "nothing was written");
}

/// A tree with no units is unservable, and a unit id that could address a path
/// outside the tree is refused.
#[test]
fn an_unservable_unit_list_is_refused() {
    let (dir, root) = objects();
    let out = dir.path().join("out");
    let mut bad = request(&root, &out, "https://bucket.example");
    bad.units = &[];
    assert!(matches!(pack(&bad), Err(PackError::Invalid("units"))));

    for (unit, object) in [
        ("../escape.c", "cores/esp32/main.c.o"),
        ("cores/esp32/main.c", "../main.c.o"),
        ("cores//main.c", "cores/esp32/main.c.o"),
        ("C:/cores/main.c", "cores/esp32/main.c.o"),
        ("", "cores/esp32/main.c.o"),
    ] {
        let units = [UnitRef { unit, object }];
        let mut bad = request(&root, &out, "https://bucket.example");
        bad.units = &units;
        assert!(
            matches!(pack(&bad), Err(PackError::Unit(_))),
            "{unit:?} -> {object:?} must be refused"
        );
    }
    assert!(!out.exists(), "a refused unit list writes nothing");
}

/// The same unit id twice is ambiguous, and a tree that already holds a
/// manifest would shadow the one the packer writes.
#[test]
fn a_duplicate_unit_or_an_existing_manifest_is_refused() {
    let (dir, root) = objects();
    let out = dir.path().join("out");
    let twice = [
        UNITS[0],
        UnitRef {
            unit: "cores/esp32/main.c",
            object: "cores/esp32/hal.c.o",
        },
    ];
    let mut bad = request(&root, &out, "https://bucket.example");
    bad.units = &twice;
    assert!(matches!(pack(&bad), Err(PackError::Unit(_))));

    std::fs::write(root.join(OBJECTS_MANIFEST).as_path(), b"{}").unwrap();
    let error = pack(&request(&root, &out, "https://bucket.example")).unwrap_err();
    assert!(matches!(
        error,
        PackError::ManifestOccupied(OBJECTS_MANIFEST)
    ));
}
