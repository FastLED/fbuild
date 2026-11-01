//! Resolution from fbuild's registry index (FastLED/fbuild#1829).

use super::*;
use crate::platformio_registry_fixture::{IndexFile, documents, serve};
use fbuild_core::path::NormalizedPath;
use fbuild_core::platformio_package::parse_package_spec;
use std::sync::atomic::Ordering;

const HASH: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const OTHER: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

fn board(owner: &'static str, version: &'static str) -> IndexFile<'static> {
    IndexFile {
        owner,
        kind: "platform",
        name: "custom-board",
        version,
        systems: &[],
        sha256: HASH,
    }
}

async fn client_for(
    files: &[IndexFile<'_>],
) -> (RegistryClient, Arc<std::sync::atomic::AtomicUsize>) {
    let (url, hits) = serve(documents(files)).await;
    (RegistryClient::new(url, reqwest::Client::new()), hits)
}

async fn resolve(
    client: &RegistryClient,
    input: &str,
    system: &str,
) -> Result<ResolvedPayload, RegistryError> {
    let spec = parse_package_spec(input).unwrap();
    client
        .resolve(spec.registry().unwrap(), PackageKind::Platform, system)
        .await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resolves_short_and_qualified_aliases_from_the_index() {
    let (client, _) = client_for(&[board("acme", "1.2.3"), board("acme", "1.3.0")]).await;
    for input in ["custom-board@1.2.3", "acme/custom-board@1.2.3"] {
        let payload = resolve(&client, input, "linux_x86_64").await.unwrap();
        assert_eq!(payload.owner, "acme");
        assert_eq!(payload.version, "1.2.3");
        assert_eq!(payload.sha256, HASH);
        assert_eq!(
            payload.url,
            "https://dl.registry.platformio.org/download/acme/platform/custom-board/1.2.3/custom-board-1.2.3.tar.gz"
        );
    }
    let latest = resolve(&client, "acme/custom-board@^1.2.0", "linux_x86_64")
        .await
        .unwrap();
    assert_eq!(latest.version, "1.3.0");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn host_specific_archives_follow_their_provides_systems() {
    let files = [
        IndexFile {
            systems: &["linux_x86_64"],
            ..board("acme", "1.0.0")
        },
        IndexFile {
            systems: &["darwin_x86_64", "darwin_arm64"],
            sha256: OTHER,
            ..board("acme", "1.0.0")
        },
    ];
    let (client, _) = client_for(&files).await;
    let linux = resolve(&client, "acme/custom-board@1.0.0", "linux_x86_64")
        .await
        .unwrap();
    assert_eq!(linux.sha256, HASH);
    assert!(matches!(
        resolve(&client, "acme/custom-board@1.0.0", "windows_amd64").await,
        Err(RegistryError::Resolution(
            ResolutionError::UnsupportedHost { .. }
        ))
    ));
    let mac = resolve(&client, "acme/custom-board@1.0.0", "darwin_arm64")
        .await
        .unwrap();
    assert_eq!(mac.sha256, OTHER);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ambiguous_short_alias_does_not_guess_an_owner() {
    let (client, _) = client_for(&[board("acme", "1.2.3"), board("other", "1.2.3")]).await;
    assert!(matches!(
        resolve(&client, "custom-board@1.2.3", "linux_x86_64").await,
        Err(RegistryError::Resolution(
            ResolutionError::AmbiguousAlias { .. }
        ))
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unindexed_packages_fail_instead_of_asking_platformio() {
    let (client, _) = client_for(&[board("acme", "1.2.3")]).await;
    for input in [
        "nobody/custom-board@1.2.3",
        "acme/missing@1.0.0",
        "missing@1.0.0",
    ] {
        assert!(
            matches!(
                resolve(&client, input, "linux_x86_64").await,
                Err(RegistryError::NotIndexed(_))
            ),
            "{input}"
        );
    }
    assert!(matches!(
        resolve(&client, "acme/custom-board@9.0.0", "linux_x86_64").await,
        Err(RegistryError::Resolution(
            ResolutionError::VersionNotFound { .. }
        ))
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unreachable_index_or_tampered_shard_is_unavailable() {
    let client = RegistryClient::new("http://127.0.0.1:1/manifest.json", reqwest::Client::new());
    assert!(matches!(
        resolve(&client, "acme/custom-board@1.2.3", "linux_x86_64").await,
        Err(RegistryError::IndexUnavailable(_))
    ));
    let mut docs = documents(&[board("acme", "1.2.3")]);
    for (path, body) in &mut docs {
        if path.contains("acme") {
            *body = String::from_utf8(body.clone())
                .unwrap()
                .replace(HASH, OTHER)
                .into_bytes();
        }
    }
    let (url, _) = serve(docs).await;
    let client = RegistryClient::new(url, reqwest::Client::new());
    assert!(matches!(
        resolve(&client, "acme/custom-board@1.2.3", "linux_x86_64").await,
        Err(RegistryError::IndexUnavailable(_))
    ));
}

/// An index published before #1829 has no names shards: owner-less specs
/// must report it as outdated, not claim the package does not exist.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pre_registry_index_is_reported_unavailable_for_owner_less_specs() {
    let docs: Vec<_> = documents(&[board("acme", "1.2.3")])
        .into_iter()
        .map(|(path, body)| {
            if path != "manifest.json" {
                return (path, body);
            }
            let mut index: serde_json::Value = serde_json::from_slice(&body).unwrap();
            let tools = index["tools"].as_object_mut().unwrap();
            tools.retain(|key, _| !key.starts_with("@names-"));
            (path, serde_json::to_vec(&index).unwrap())
        })
        .collect();
    let (url, _) = serve(docs).await;
    let client = RegistryClient::new(url, reqwest::Client::new());
    assert!(matches!(
        resolve(&client, "custom-board@1.2.3", "linux_x86_64").await,
        Err(RegistryError::IndexUnavailable(_))
    ));
    assert!(
        resolve(&client, "acme/custom-board@1.2.3", "linux_x86_64")
            .await
            .is_ok()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cached_payload_allows_offline_checks_and_warm_resolution() {
    let (client, hits) = client_for(&[board("acme", "1.2.3")]).await;
    let spec = parse_package_spec("acme/custom-board@1.2.3").unwrap();
    let registry = spec.registry().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let cached = |system: &'static str, fetch: bool| {
        client.resolve_cached(registry, PackageKind::Platform, system, cache.path(), fetch)
    };
    assert!(cached("linux_x86_64", false).await.unwrap().is_none());
    assert_eq!(hits.load(Ordering::SeqCst), 0, "offline checks never fetch");
    let first = cached("linux_x86_64", true).await.unwrap().unwrap();
    let fetched = hits.load(Ordering::SeqCst);
    assert_eq!(fetched, 2, "the index and one owner shard");
    assert_eq!(
        cached("linux_x86_64", false).await.unwrap(),
        Some(first.clone())
    );
    assert_eq!(cached("linux_x86_64", true).await.unwrap(), Some(first));
    assert_eq!(
        hits.load(Ordering::SeqCst),
        fetched,
        "exact pins stay cached"
    );
    assert!(cached("windows_amd64", false).await.unwrap().is_none());
}

/// #1829: nothing in fbuild's runtime code may query PlatformIO's registry
/// API; resolution reads fbuild's own index.
#[test]
fn no_runtime_code_references_the_platformio_registry_api() {
    let crates = NormalizedPath::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let banned = ["api.registry", ".platformio.org"].concat();
    let mut offenders = Vec::new();
    let mut stack = vec![crates];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(dir.as_path()).unwrap().flatten() {
            let path = NormalizedPath::new(entry.path());
            let name = entry.file_name().to_string_lossy().into_owned();
            if path.as_path().is_dir() {
                if name != "target" && name != "tests" {
                    stack.push(path);
                }
            } else if name.ends_with(".rs")
                && !name.ends_with("_tests.rs")
                && std::fs::read_to_string(path.as_path())
                    .unwrap()
                    .contains(&banned)
            {
                offenders.push(path.display_slash());
            }
        }
    }
    assert!(offenders.is_empty(), "{offenders:?}");
}
