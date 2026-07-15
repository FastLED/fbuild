//! Typed USB transport profiles downloaded from FastLED/boards.

use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::RwLock;
use std::time::Duration;

use super::data::ONLINE_CACHE_TTL_SECS;

pub const USB_PROFILES_SCHEMA_VERSION: u64 = 1;
pub const USB_PROFILES_URL: &str = "https://fastled.github.io/boards/usb-profiles.json";
pub const USB_PROFILES_META_URL: &str = "https://fastled.github.io/boards/_meta.json";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UsbPurpose {
    Compile,
    Runtime,
    Bootloader,
    Probe,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UsbDeviceRole {
    RuntimeCdc,
    UsbUartBridge,
    BootloaderMsc,
    BootloaderHid,
    BootloaderDfu,
    BootloaderUf2,
    DebugProbe,
    RecoveryTransport,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct UsbIdentityMatch {
    pub vid: String,
    pub pid: Option<String>,
    pub pid_mask: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct UsbProfileProvenance {
    pub source_url: String,
    pub source_revision: String,
    pub source_class: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct UsbTransportProfile {
    #[serde(rename = "match")]
    pub identity_match: UsbIdentityMatch,
    pub purpose: UsbPurpose,
    pub role: UsbDeviceRole,
    pub transport: String,
    pub reset: String,
    pub handoff: String,
    pub platform: Option<String>,
    pub family: Option<String>,
    pub generation: Option<String>,
    pub interface: Option<String>,
    pub provenance: UsbProfileProvenance,
    pub priority: u16,
    pub allow_ambiguous: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoardUsbProfile {
    pub board_id: String,
    pub identities: BTreeMap<String, Vec<String>>,
    pub aliases: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct PublishedMeta {
    usb_profiles: String,
    usb_profiles_schema_version: u64,
    usb_profiles_sha256: String,
}

#[derive(Debug, Deserialize)]
struct PublishedArtifact {
    schema_version: u64,
    metadata: serde_json::Value,
    identities: BTreeMap<String, Vec<UsbTransportProfile>>,
    boards: BTreeMap<String, PublishedBoardProfile>,
}

#[derive(Debug, Deserialize)]
struct PublishedBoardProfile {
    identities: BTreeMap<String, Vec<String>>,
    aliases: Vec<String>,
}

#[derive(Debug)]
struct IndexedProfile {
    vid: u16,
    pid: Option<u16>,
    pid_mask: Option<u16>,
    profile: UsbTransportProfile,
}

#[derive(Debug, Default)]
struct InstalledProfiles {
    identities: Vec<IndexedProfile>,
    boards: HashMap<String, BoardUsbProfile>,
    aliases: HashMap<String, String>,
    ambiguous_aliases: HashSet<String>,
}

static INSTALLED: RwLock<Option<InstalledProfiles>> = RwLock::new(None);
static CACHE_WRITE_COUNTER: AtomicU64 = AtomicU64::new(0);

pub fn profiles_for(vid: u16, pid: u16) -> Vec<UsbTransportProfile> {
    let Ok(guard) = INSTALLED.read() else {
        return Vec::new();
    };
    let Some(installed) = guard.as_ref() else {
        return Vec::new();
    };
    let mut profiles: Vec<_> = installed
        .identities
        .iter()
        .filter(|entry| entry.vid == vid && identity_pid_matches(entry, pid))
        .map(|entry| entry.profile.clone())
        .collect();
    profiles.sort_by(|left, right| right.priority.cmp(&left.priority));
    profiles
}

fn identity_pid_matches(entry: &IndexedProfile, candidate: u16) -> bool {
    let Some(expected) = entry.pid else {
        return true;
    };
    match entry.pid_mask {
        Some(mask) => candidate & mask == expected & mask,
        None => candidate == expected,
    }
}

pub fn board_profile(board_or_alias: &str) -> Option<BoardUsbProfile> {
    let exact = board_or_alias.trim();
    let guard = INSTALLED.read().ok()?;
    let installed = guard.as_ref()?;
    if let Some(profile) = installed.boards.get(exact) {
        return Some(profile.clone());
    }
    let key = exact.to_ascii_lowercase();
    if installed.ambiguous_aliases.contains(&key) {
        return None;
    }
    let board_id = installed.aliases.get(&key)?;
    installed.boards.get(board_id).cloned()
}

pub fn try_install_verified_cache(meta_path: &Path, profiles_path: &Path) -> Result<usize, String> {
    let meta = std::fs::read(meta_path).map_err(|error| format!("read metadata: {error}"))?;
    let profiles =
        std::fs::read(profiles_path).map_err(|error| format!("read profiles: {error}"))?;
    let installed = decode_verified(&meta, &profiles)?;
    let count = installed.identities.len();
    install(installed);
    Ok(count)
}

pub fn populate_profiles_from_paths(meta_path: &Path, profiles_path: &Path) -> bool {
    populate_profiles_from_paths_and_urls(
        meta_path,
        profiles_path,
        USB_PROFILES_META_URL,
        USB_PROFILES_URL,
    )
}

pub fn populate_profiles_from_paths_and_urls(
    meta_path: &Path,
    profiles_path: &Path,
    meta_url: &str,
    profiles_url: &str,
) -> bool {
    if cache_is_fresh(meta_path)
        && cache_is_fresh(profiles_path)
        && try_install_verified_cache(meta_path, profiles_path).is_ok()
    {
        return true;
    }

    match fetch_verified_pair(meta_url, profiles_url) {
        Ok((meta, profiles, installed)) => {
            if let Err(error) = write_cache_pair(meta_path, profiles_path, &meta, &profiles) {
                tracing::warn!(error = %error, "USB profile cache write failed; using verified in-memory data");
            }
            install(installed);
            true
        }
        Err(error) => {
            tracing::warn!(error = %error, "verified FastLED/boards USB profiles unavailable");
            try_install_verified_cache(meta_path, profiles_path).is_ok()
        }
    }
}

fn fetch_verified_pair(
    meta_url: &str,
    profiles_url: &str,
) -> Result<(Vec<u8>, Vec<u8>, InstalledProfiles), String> {
    let client = crate::http::blocking_client(Duration::from_secs(15));
    let meta = fetch_bytes(&client, meta_url)?;
    // Metadata is deliberately fetched first. Its schema and digest bind the
    // second response so a site rebuild between requests fails closed.
    let parsed_meta: PublishedMeta = serde_json::from_slice(&meta)
        .map_err(|error| format!("metadata JSON: {error}"))?;
    validate_meta(&parsed_meta)?;
    let profiles = fetch_bytes(&client, profiles_url)?;
    let installed = decode_verified(&meta, &profiles)?;
    Ok((meta, profiles, installed))
}

fn fetch_bytes(client: &reqwest::blocking::Client, url: &str) -> Result<Vec<u8>, String> {
    let response = client
        .get(url)
        .send()
        .map_err(|error| format!("GET {url}: {error}"))?;
    if !response.status().is_success() {
        return Err(format!("GET {url}: HTTP {}", response.status()));
    }
    Ok(response
        .bytes()
        .map_err(|error| format!("read {url}: {error}"))?
        .to_vec())
}

fn decode_verified(meta_bytes: &[u8], profile_bytes: &[u8]) -> Result<InstalledProfiles, String> {
    let meta: PublishedMeta = serde_json::from_slice(meta_bytes)
        .map_err(|error| format!("metadata JSON: {error}"))?;
    validate_meta(&meta)?;

    let value: serde_json::Value = serde_json::from_slice(profile_bytes)
        .map_err(|error| format!("profile JSON: {error}"))?;
    let canonical = serde_json::to_vec(&value)
        .map_err(|error| format!("canonical profile JSON: {error}"))?;
    let actual_sha = hex_sha256(&canonical);
    if actual_sha != meta.usb_profiles_sha256 {
        return Err(format!(
            "USB profile sha256 mismatch: metadata={}, artifact={actual_sha}",
            meta.usb_profiles_sha256
        ));
    }

    let artifact: PublishedArtifact = serde_json::from_value(value)
        .map_err(|error| format!("USB profile schema: {error}"))?;
    validate_and_index(artifact)
}

fn validate_meta(meta: &PublishedMeta) -> Result<(), String> {
    if meta.usb_profiles_schema_version != USB_PROFILES_SCHEMA_VERSION {
        return Err(format!(
            "unsupported USB profile schema {} (expected {})",
            meta.usb_profiles_schema_version, USB_PROFILES_SCHEMA_VERSION
        ));
    }
    if meta.usb_profiles != "usb-profiles.json" {
        return Err(format!(
            "unexpected USB profile artifact name {:?}",
            meta.usb_profiles
        ));
    }
    if !is_lower_hex(&meta.usb_profiles_sha256, 64) {
        return Err("metadata contains an invalid usb_profiles_sha256".to_string());
    }
    Ok(())
}

fn validate_and_index(artifact: PublishedArtifact) -> Result<InstalledProfiles, String> {
    if artifact.schema_version != USB_PROFILES_SCHEMA_VERSION {
        return Err(format!(
            "artifact schema {} does not match supported schema {}",
            artifact.schema_version, USB_PROFILES_SCHEMA_VERSION
        ));
    }
    if !artifact.metadata.is_object() {
        return Err("USB profile metadata is not an object".to_string());
    }

    let mut installed = InstalledProfiles::default();
    for (identity_key, profiles) in &artifact.identities {
        let (key_vid, key_pid) = parse_identity_key(identity_key)?;
        for profile in profiles {
            let vid = parse_hex(&profile.identity_match.vid, "match VID")?;
            let pid = profile
                .identity_match
                .pid
                .as_deref()
                .map(|value| parse_hex(value, "match PID"))
                .transpose()?;
            let pid_mask = profile
                .identity_match
                .pid_mask
                .as_deref()
                .map(|value| parse_hex(value, "PID mask"))
                .transpose()?;
            if vid != key_vid || pid != key_pid {
                return Err(format!("identity key {identity_key} disagrees with match fields"));
            }
            if pid_mask == Some(0) {
                return Err(format!("identity {identity_key} has a zero PID mask"));
            }
            if profile.provenance.source_url.trim().is_empty()
                || profile.provenance.source_class.trim().is_empty()
                || !matches!(profile.provenance.source_revision.len(), 40 | 64)
                || !profile.provenance.source_revision.bytes().all(|byte| byte.is_ascii_hexdigit())
            {
                return Err(format!("identity {identity_key} has invalid provenance"));
            }
            installed.identities.push(IndexedProfile {
                vid,
                pid,
                pid_mask,
                profile: profile.clone(),
            });
        }
    }

    for (board_id, profile) in artifact.boards {
        if board_id.trim().is_empty() {
            return Err("USB board profile has an empty board ID".to_string());
        }
        for keys in profile.identities.values() {
            for key in keys {
                if !artifact.identities.contains_key(key) {
                    return Err(format!("board {board_id} references unknown identity {key}"));
                }
            }
        }
        let normalized_id = board_id.to_ascii_lowercase();
        index_alias(&mut installed, normalized_id, &board_id);
        for alias in &profile.aliases {
            let normalized_alias = alias.trim().to_ascii_lowercase();
            if !normalized_alias.is_empty() {
                index_alias(&mut installed, normalized_alias, &board_id);
            }
        }
        installed.boards.insert(
            board_id.clone(),
            BoardUsbProfile {
                board_id,
                identities: profile.identities,
                aliases: profile.aliases,
            },
        );
    }
    Ok(installed)
}

fn index_alias(installed: &mut InstalledProfiles, alias: String, board_id: &str) {
    if installed.ambiguous_aliases.contains(&alias) {
        return;
    }
    if installed
        .aliases
        .get(&alias)
        .is_some_and(|existing| existing != board_id)
    {
        installed.aliases.remove(&alias);
        installed.ambiguous_aliases.insert(alias);
    } else {
        installed.aliases.insert(alias, board_id.to_string());
    }
}

fn parse_identity_key(value: &str) -> Result<(u16, Option<u16>), String> {
    let Some((vid, pid)) = value.split_once(':') else {
        return Err(format!("invalid USB identity key {value:?}"));
    };
    let vid = parse_hex(vid, "identity VID")?;
    let pid = if pid == "*" {
        None
    } else {
        Some(parse_hex(pid, "identity PID")?)
    };
    Ok((vid, pid))
}

fn parse_hex(value: &str, label: &str) -> Result<u16, String> {
    if !is_lower_hex(value, 4) {
        return Err(format!("invalid {label} {value:?}"));
    }
    u16::from_str_radix(value, 16).map_err(|error| format!("invalid {label}: {error}"))
}

fn is_lower_hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

fn hex_sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn install(profiles: InstalledProfiles) {
    let mut guard = INSTALLED.write().unwrap_or_else(|error| error.into_inner());
    *guard = Some(profiles);
}

fn cache_is_fresh(path: &Path) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    let Ok(modified) = metadata.modified() else {
        return false;
    };
    modified
        .elapsed()
        .is_ok_and(|age| age.as_secs() < ONLINE_CACHE_TTL_SECS)
}

fn write_cache_pair(
    meta_path: &Path,
    profiles_path: &Path,
    meta: &[u8],
    profiles: &[u8],
) -> Result<(), String> {
    let counter = CACHE_WRITE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let suffix = format!("tmp-{}-{counter}", std::process::id());
    let meta_tmp = meta_path.with_extension(&suffix);
    let profiles_tmp = profiles_path.with_extension(&suffix);
    for path in [meta_path, profiles_path] {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| format!("mkdir: {error}"))?;
        }
    }
    std::fs::write(&meta_tmp, meta).map_err(|error| format!("write metadata: {error}"))?;
    std::fs::write(&profiles_tmp, profiles)
        .map_err(|error| format!("write profiles: {error}"))?;
    // Publish the artifact first and metadata last. A crash can therefore
    // leave only a mismatched pair, which validation rejects on next use.
    publish_cache_file(&profiles_tmp, profiles_path, "profiles")?;
    publish_cache_file(&meta_tmp, meta_path, "metadata")?;
    Ok(())
}

fn publish_cache_file(source: &Path, destination: &Path, label: &str) -> Result<(), String> {
    match std::fs::rename(source, destination) {
        Ok(()) => Ok(()),
        Err(first_error) if destination.is_file() => {
            // Windows rename does not replace an existing file. These files
            // are a verified cache; a crash between removal and rename only
            // causes the next launch to download the pair again.
            std::fs::remove_file(destination)
                .map_err(|error| format!("replace old {label} cache: {error}"))?;
            std::fs::rename(source, destination)
                .map_err(|error| format!("publish {label}: {error} (initial error: {first_error})"))
        }
        Err(error) => Err(format!("publish {label}: {error}")),
    }
}

#[cfg(test)]
fn clear_profiles_for_tests() {
    let mut guard = INSTALLED.write().unwrap_or_else(|error| error.into_inner());
    *guard = None;
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::Mutex;

    static PROFILE_TEST_LOCK: Mutex<()> = Mutex::new(());

    const SYNTHETIC_ARTIFACT: &str = r#"{"boards":{"synthetic-board":{"aliases":["synthetic-alias"],"identities":{"bootloader":[],"compile":[],"probe":[],"runtime":["feed:c0de"]}}},"identities":{"feed:c0de":[{"allow_ambiguous":false,"family":"synthetic-family","generation":"one","handoff":"bootloader","interface":"cdc","match":{"pid":"c0de","pid_mask":null,"vid":"feed"},"platform":"synthetic","priority":100,"provenance":{"source_class":"test","source_revision":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","source_url":"test://fixture"},"purpose":"runtime","reset":"touch-1200","role":"runtime_cdc","transport":"usb"}]},"metadata":{"artifact":"usb-transport-profiles","compatibility":{}},"schema_version":1}"#;

    fn fixture_meta(artifact: &str, schema: u64) -> String {
        let value: serde_json::Value = serde_json::from_str(artifact).unwrap();
        let canonical = serde_json::to_vec(&value).unwrap();
        let digest = Sha256::digest(canonical);
        let hash = digest.iter().map(|byte| format!("{byte:02x}")).collect::<String>();
        format!(
            r#"{{"usb_profiles":"usb-profiles.json","usb_profiles_schema_version":{schema},"usb_profiles_sha256":"{hash}"}}"#
        )
    }

    #[test]
    fn verified_cache_installs_typed_profiles_and_aliases() {
        let _guard = PROFILE_TEST_LOCK.lock().unwrap();
        clear_profiles_for_tests();
        let temp = tempfile::tempdir().unwrap();
        let meta = temp.path().join("_meta.json");
        let artifact = temp.path().join("usb-profiles.json");
        std::fs::write(&meta, fixture_meta(SYNTHETIC_ARTIFACT, 1)).unwrap();
        std::fs::write(&artifact, SYNTHETIC_ARTIFACT).unwrap();

        assert_eq!(try_install_verified_cache(&meta, &artifact).unwrap(), 1);
        let profiles = profiles_for(0xfeed, 0xc0de);
        assert_eq!(profiles.len(), 1);
        assert_eq!(profiles[0].role, UsbDeviceRole::RuntimeCdc);
        assert_eq!(profiles[0].family.as_deref(), Some("synthetic-family"));
        assert_eq!(board_profile("synthetic-alias").unwrap().board_id, "synthetic-board");
    }

    #[test]
    fn ambiguous_casefolded_board_ids_and_aliases_fail_closed() {
        let _guard = PROFILE_TEST_LOCK.lock().unwrap();
        clear_profiles_for_tests();
        let mut value: serde_json::Value = serde_json::from_str(SYNTHETIC_ARTIFACT).unwrap();
        let template = value["boards"]["synthetic-board"].clone();
        value["boards"] = serde_json::json!({
            "CaseBoard": template,
            "caseboard": {
                "aliases": ["synthetic-alias"],
                "identities": {
                    "bootloader": [],
                    "compile": [],
                    "probe": [],
                    "runtime": ["feed:c0de"]
                }
            }
        });
        let artifact = serde_json::to_string(&value).unwrap();
        let temp = tempfile::tempdir().unwrap();
        let meta = temp.path().join("_meta.json");
        let profiles = temp.path().join("usb-profiles.json");
        std::fs::write(&meta, fixture_meta(&artifact, 1)).unwrap();
        std::fs::write(&profiles, artifact).unwrap();

        try_install_verified_cache(&meta, &profiles).unwrap();
        assert_eq!(board_profile("CaseBoard").unwrap().board_id, "CaseBoard");
        assert_eq!(board_profile("caseboard").unwrap().board_id, "caseboard");
        assert!(board_profile("CASEBOARD").is_none());
        assert!(board_profile("synthetic-alias").is_none());
    }

    #[test]
    fn cache_refresh_replaces_an_existing_verified_pair() {
        let temp = tempfile::tempdir().unwrap();
        let meta = temp.path().join("_meta.json");
        let profiles = temp.path().join("usb-profiles.json");
        std::fs::write(&meta, b"old-meta").unwrap();
        std::fs::write(&profiles, b"old-profiles").unwrap();

        write_cache_pair(&meta, &profiles, b"new-meta", b"new-profiles").unwrap();

        assert_eq!(std::fs::read(meta).unwrap(), b"new-meta");
        assert_eq!(std::fs::read(profiles).unwrap(), b"new-profiles");
    }

    #[test]
    #[ignore = "live FastLED/boards publication smoke test"]
    fn live_publication_verifies_and_exposes_curated_pico_roles() {
        let _guard = PROFILE_TEST_LOCK.lock().unwrap();
        clear_profiles_for_tests();
        let temp = tempfile::tempdir().unwrap();
        let meta = temp.path().join("_meta.json");
        let profiles = temp.path().join("usb-profiles.json");

        assert!(populate_profiles_from_paths(&meta, &profiles));
        assert!(profiles_for(0x2e8a, 0x000a).iter().any(|profile| {
            profile.purpose == UsbPurpose::Runtime
                && profile.role == UsbDeviceRole::RuntimeCdc
                && profile.family.as_deref() == Some("rp2040")
                && profile.interface.as_deref() == Some("cdc")
        }));
        assert!(profiles_for(0x2e8a, 0x0003).iter().any(|profile| {
            profile.purpose == UsbPurpose::Bootloader
                && profile.role == UsbDeviceRole::BootloaderUf2
                && profile.family.as_deref() == Some("rp2040")
                && profile.interface.as_deref() == Some("msc")
        }));
    }

    #[test]
    fn tampered_artifact_and_wrong_schema_are_rejected() {
        let _guard = PROFILE_TEST_LOCK.lock().unwrap();
        clear_profiles_for_tests();
        let temp = tempfile::tempdir().unwrap();
        let meta = temp.path().join("_meta.json");
        let artifact = temp.path().join("usb-profiles.json");
        std::fs::write(&meta, fixture_meta(SYNTHETIC_ARTIFACT, 1)).unwrap();
        std::fs::write(&artifact, SYNTHETIC_ARTIFACT.replace("synthetic-family", "tampered")).unwrap();
        assert!(try_install_verified_cache(&meta, &artifact)
            .unwrap_err()
            .contains("sha256"));
        assert!(profiles_for(0xfeed, 0xc0de).is_empty());

        std::fs::write(&meta, fixture_meta(SYNTHETIC_ARTIFACT, 2)).unwrap();
        std::fs::write(&artifact, SYNTHETIC_ARTIFACT).unwrap();
        assert!(try_install_verified_cache(&meta, &artifact)
            .unwrap_err()
            .contains("schema"));
    }

    #[test]
    fn fetch_rejects_bad_metadata_before_requesting_artifact() {
        let _guard = PROFILE_TEST_LOCK.lock().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 1024];
            let _ = stream.read(&mut request).unwrap();
            let body = r#"{"usb_profiles":"usb-profiles.json","usb_profiles_schema_version":2,"usb_profiles_sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}"#;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
            stream.flush().unwrap();
            listener.set_nonblocking(true).unwrap();
            let deadline = std::time::Instant::now() + Duration::from_millis(300);
            let mut extra_requests = 0;
            while std::time::Instant::now() < deadline {
                match listener.accept() {
                    Ok(_) => extra_requests += 1,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    Err(error) => panic!("accept failed: {error}"),
                }
            }
            extra_requests
        });

        let error = fetch_verified_pair(
            &format!("http://{address}/_meta.json"),
            &format!("http://{address}/usb-profiles.json"),
        )
        .unwrap_err();
        assert!(error.contains("schema"));
        assert_eq!(server.join().unwrap(), 0);
    }
}
