//! A registry index served from loopback, for tests here and in dependent
//! crates. Documents follow `ci/gen_mirror_manifest.py`'s layout.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::downloader::{names_key, owner_key};

/// One archive the index lists.
pub struct IndexFile<'a> {
    pub owner: &'a str,
    /// Registry type: `platform`, `tool` or `library`.
    pub kind: &'a str,
    pub name: &'a str,
    pub version: &'a str,
    /// PlatformIO `system` values; empty means any host.
    pub systems: &'a [&'a str],
    pub sha256: &'a str,
}

/// `(path, body)` for the Index (`manifest.json`), each owner's shard and the
/// `@names-<h>` shards.
#[must_use]
pub fn documents(files: &[IndexFile<'_>]) -> Vec<(String, Vec<u8>)> {
    let mut by_owner: BTreeMap<&str, Vec<serde_json::Value>> = BTreeMap::new();
    let mut names: BTreeMap<String, Vec<serde_json::Value>> = BTreeMap::new();
    for file in files {
        let suffix: String = file.systems.iter().map(|s| format!("-{s}")).collect();
        let id = format!(
            "{}/{}/{}/{}/{}-{}{suffix}.tar.gz",
            file.owner, file.kind, file.name, file.version, file.name, file.version
        );
        let mut asset = serde_json::json!({
            "sha256": file.sha256,
            "urls": [
                format!("https://fbuild.sfo3.digitaloceanspaces.com/{id}"),
                format!("https://dl.registry.platformio.org/download/{id}"),
            ],
        });
        if !file.systems.is_empty() {
            asset["provides"] = serde_json::json!(file.systems);
        }
        let package = format!("{}/{}/{}", file.kind, file.name, file.owner);
        let listed = names.entry(names_key(file.name)).or_default();
        if !listed.iter().any(|c| c["id"] == package.as_str()) {
            listed.push(serde_json::json!({ "id": package, "asset": asset.clone() }));
        }
        by_owner
            .entry(file.owner)
            .or_default()
            .push(serde_json::json!({ "id": id, "asset": asset }));
    }
    let mut docs = Vec::new();
    let mut tools = serde_json::Map::new();
    let mut add = |key: String, components: Vec<serde_json::Value>| {
        let path = format!("shards/{}.json", key.replace(['@', '%'], "_"));
        let body = serde_json::to_vec(&serde_json::json!({
            "kind": "Release",
            "schema_version": 1,
            "components": components,
        }))
        .expect("fixture shard serializes");
        tools.insert(
            key,
            serde_json::json!({ "descriptor": {
                "url": path,
                "sha256": format!("{:x}", Sha256::digest(&body)),
                "size_bytes": body.len(),
            }}),
        );
        docs.push((path, body));
    };
    for (owner, components) in by_owner {
        add(owner_key(owner), components);
    }
    for (key, components) in names {
        add(key, components);
    }
    let index = serde_json::json!({ "kind": "Index", "schema_version": 1, "tools": tools });
    docs.push((
        "manifest.json".to_string(),
        serde_json::to_vec(&index).expect("fixture index serializes"),
    ));
    docs
}

/// Serve `docs` over loopback HTTP. Returns the Index URL and a count of
/// requests answered.
pub async fn serve(docs: Vec<(String, Vec<u8>)>) -> (String, Arc<AtomicUsize>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind loopback");
    let address = listener.local_addr().expect("local address");
    let hits = Arc::new(AtomicUsize::new(0));
    let docs = Arc::new(docs);
    let counter = hits.clone();
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            let docs = docs.clone();
            let counter = counter.clone();
            tokio::spawn(async move {
                let mut request = vec![0_u8; 8192];
                let Ok(read) = socket.read(&mut request).await else {
                    return;
                };
                let line = String::from_utf8_lossy(&request[..read]);
                let path = line.split_whitespace().nth(1).unwrap_or("/");
                counter.fetch_add(1, Ordering::SeqCst);
                let response = match docs.iter().find(|(p, _)| path == format!("/{p}")) {
                    Some((_, body)) => {
                        let mut head = format!(
                            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            body.len()
                        )
                        .into_bytes();
                        head.extend_from_slice(body);
                        head
                    }
                    None => {
                        b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                            .to_vec()
                    }
                };
                let _ = socket.write_all(&response).await;
            });
        }
    });
    (format!("http://{address}/manifest.json"), hits)
}
