//! Real tar.zst fixtures for the prebuilt extraction cell (#1882).
use super::*;
use sha2::{Digest, Sha256};
use std::io::Write;
use tar::{Builder, EntryType, Header};

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// One fixture entry: path, contents, and the entry type tar should store.
#[derive(Clone)]
enum Item {
    File(&'static str, Vec<u8>),
    Dir(&'static str),
    Symlink(&'static str, &'static str),
    /// A name written into the raw header: absolute or traversing.
    Raw(&'static str, Vec<u8>),
}

fn tar_zst(items: &[Item]) -> Vec<u8> {
    let encoder = zstd::Encoder::new(Vec::new(), 3).unwrap();
    let mut builder = Builder::new(encoder);
    for item in items {
        match item {
            Item::File(path, body) => {
                let mut header = Header::new_gnu();
                header.set_size(body.len() as u64);
                header.set_mode(0o644);
                header.set_entry_type(EntryType::Regular);
                header.set_cksum();
                builder
                    .append_data(&mut header, path, body.as_slice())
                    .unwrap();
            }
            Item::Dir(path) => {
                let mut header = Header::new_gnu();
                header.set_size(0);
                header.set_mode(0o755);
                header.set_entry_type(EntryType::Directory);
                header.set_cksum();
                builder
                    .append_data(&mut header, path, std::io::empty())
                    .unwrap();
            }
            Item::Symlink(path, target) => {
                let mut header = Header::new_gnu();
                header.set_size(0);
                header.set_mode(0o777);
                header.set_entry_type(EntryType::Symlink);
                header
                    .set_link_name(target)
                    .expect("fixture link target is a valid name");
                header.set_cksum();
                builder
                    .append_data(&mut header, path, std::io::empty())
                    .unwrap();
            }
            Item::Raw(path, body) => {
                let header = raw_header(path, body.len() as u64);
                builder.append(&header, body.as_slice()).unwrap();
            }
        }
    }
    let mut encoder = builder.into_inner().unwrap();
    encoder.flush().unwrap();
    encoder.finish().unwrap()
}

/// A header whose stored name is whatever the fixture asks for. `Header::set_path`
/// refuses absolute names and `..`, so the name and checksum are written into
/// the raw block directly — the exact hostile shape a real archive can still
/// carry while still being settable by other tools.
fn raw_header(name: &str, size: u64) -> Header {
    let mut header = Header::new_gnu();
    header.set_size(size);
    header.set_mode(0o644);
    header.set_entry_type(EntryType::Regular);
    assert!(name.len() < 100);
    let bytes = header.as_mut_bytes();
    bytes[..name.len()].copy_from_slice(name.as_bytes());
    // The checksum field is summed as spaces, then written back.
    for slot in bytes[148..156].iter_mut() {
        *slot = b' ';
    }
    let sum: u32 = bytes.iter().map(|byte| u32::from(*byte)).sum();
    let text = format!("{:06o}\0 ", sum);
    bytes[148..156].copy_from_slice(text.as_bytes());
    header
}

fn artifact(dir: &Path, name: &str, bytes: &[u8]) -> Artifact {
    let path = dir.join(name);
    std::fs::write(&path, bytes).unwrap();
    Artifact {
        path: NormalizedPath::new(&path),
        size_bytes: bytes.len() as u64,
        sha256: digest(bytes),
    }
}

/// A destination plus its parent, both under one tempdir.
fn layout() -> (tempfile::TempDir, NormalizedPath) {
    let tmp = tempfile::tempdir().unwrap();
    let dest = NormalizedPath::new(tmp.path()).join("tree");
    (tmp, dest)
}

/// Leftover staging directories in the destination's parent, sorted. The
/// staging name is the only thing a refused extraction may leave behind.
fn staging_debris(dest: &NormalizedPath) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dest.as_path().parent().unwrap())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with('.'))
        .collect();
    names.sort();
    names
}

#[test]
fn extracts_every_file_and_directory() {
    let (tmp, dest) = layout();
    let bytes = tar_zst(&[
        Item::Dir("fw"),
        Item::File("fw/a.txt", b"alpha".to_vec()),
        Item::File("fw/sub/b.bin", b"bravo-bytes".to_vec()),
    ]);
    let artifact = artifact(tmp.path(), "prebuilt.tar.zst", &bytes);

    let tree = extract(&artifact, dest.as_path()).unwrap();

    assert_eq!(tree.root, dest);
    assert_eq!(tree.entries, 3);
    assert_eq!(
        std::fs::read(dest.join("fw/a.txt").as_path()).unwrap(),
        b"alpha"
    );
    assert_eq!(
        std::fs::read(dest.join("fw/sub/b.bin").as_path()).unwrap(),
        b"bravo-bytes"
    );
    let marker = std::fs::read(dest.join(MARKER).as_path()).unwrap();
    let marker: TreeMarker = serde_json::from_slice(&marker).unwrap();
    assert_eq!(marker.sha256, artifact.sha256);
    assert_eq!(marker.size_bytes, artifact.size_bytes);
    assert_eq!(marker.extracted_bytes, tree.bytes);
}

#[test]
fn a_completed_tree_is_reused_without_the_archive() {
    let (tmp, dest) = layout();
    let bytes = tar_zst(&[Item::File("keep.txt", b"cached".to_vec())]);
    let artifact = artifact(tmp.path(), "prebuilt.tar.zst", &bytes);
    let fresh = extract(&artifact, dest.as_path()).unwrap();

    // The archive no longer exists, so a second extraction can only succeed
    // through the marker.
    std::fs::remove_file(artifact.path.as_path()).unwrap();
    let tree = extract(&artifact, dest.as_path()).unwrap();

    assert_eq!(tree.entries, 1);
    assert_eq!(
        std::fs::read(dest.join("keep.txt").as_path()).unwrap(),
        b"cached"
    );
    // A reuse reports the same numbers a fresh extraction did: the marker must
    // carry the tree's byte count, not the archive's.
    assert_eq!(tree, fresh);
    assert_eq!(tree.bytes, b"cached".len() as u64);
}

#[test]
fn a_flipped_body_is_refused_and_leaves_nothing() {
    let (tmp, dest) = layout();
    let mut bytes = tar_zst(&[Item::File("a.txt", b"payload".to_vec())]);
    let artifact = artifact(tmp.path(), "prebuilt.tar.zst", &bytes);
    let last = bytes.len() - 1;
    bytes[last] ^= 0xff;
    std::fs::write(artifact.path.as_path(), &bytes).unwrap();

    let error = extract(&artifact, dest.as_path()).unwrap_err();

    assert!(
        matches!(
            error,
            ExtractError::Integrity { .. } | ExtractError::Archive(_)
        ),
        "{error:?}"
    );
    assert!(!exists(&dest), "a refused archive must not leave a tree");
    assert_eq!(
        staging_debris(&dest),
        Vec::<String>::new(),
        "no staging debris"
    );
}

/// A body that is still a valid tar.zst stream but is no longer the archive the
/// descriptor named must be caught by the digest, not by the decoder.
#[test]
fn a_replaced_body_fails_the_descriptor_check() {
    let (tmp, dest) = layout();
    let honest = tar_zst(&[Item::File("a.txt", b"payload".to_vec())]);
    let mut artifact = artifact(tmp.path(), "prebuilt.tar.zst", &honest);
    let other = tar_zst(&[Item::File("a.txt", b"different".to_vec())]);
    std::fs::write(artifact.path.as_path(), &other).unwrap();
    artifact.size_bytes = other.len() as u64;

    let error = extract(&artifact, dest.as_path()).unwrap_err();

    assert!(matches!(error, ExtractError::Integrity { .. }), "{error:?}");
    assert!(!exists(&dest));
    assert_eq!(staging_debris(&dest), Vec::<String>::new());
}

#[test]
fn a_truncated_body_is_refused() {
    let (tmp, dest) = layout();
    let bytes = tar_zst(&[Item::File("a.txt", b"payload".to_vec())]);
    let artifact = artifact(tmp.path(), "prebuilt.tar.zst", &bytes);
    std::fs::write(artifact.path.as_path(), &bytes[..bytes.len() / 2]).unwrap();

    let error = extract(&artifact, dest.as_path()).unwrap_err();

    assert!(!matches!(error, ExtractError::Unsupported(_)), "{error:?}");
    assert!(!exists(&dest));
    assert_eq!(staging_debris(&dest), Vec::<String>::new());
}

#[test]
fn parent_traversal_is_refused() {
    let (tmp, dest) = layout();
    let bytes = tar_zst(&[Item::Raw("../escape.txt", b"nope".to_vec())]);
    let artifact = artifact(tmp.path(), "prebuilt.tar.zst", &bytes);

    let error = extract(&artifact, dest.as_path()).unwrap_err();

    assert!(matches!(error, ExtractError::Entry(_)), "{error:?}");
    assert!(!tmp.path().join("escape.txt").exists());
    assert!(!exists(&dest));
}

#[test]
fn an_absolute_path_is_refused() {
    let (tmp, dest) = layout();
    let bytes = tar_zst(&[Item::Raw("/etc/fbuild-prebuilt-escape", b"nope".to_vec())]);
    let artifact = artifact(tmp.path(), "prebuilt.tar.zst", &bytes);

    let error = extract(&artifact, dest.as_path()).unwrap_err();

    assert!(matches!(error, ExtractError::Entry(_)), "{error:?}");
    assert!(!exists(&dest));
}

#[test]
fn a_symlink_is_refused() {
    let (tmp, dest) = layout();
    let bytes = tar_zst(&[Item::Symlink("link", "/etc/passwd")]);
    let artifact = artifact(tmp.path(), "prebuilt.tar.zst", &bytes);

    let error = extract(&artifact, dest.as_path()).unwrap_err();

    assert!(
        matches!(error, ExtractError::EntryType(_, "symlink")),
        "{error:?}"
    );
    assert!(!exists(&dest));
}

#[test]
fn an_occupied_destination_is_never_merged_into() {
    let (tmp, dest) = layout();
    std::fs::create_dir_all(dest.join("existing").as_path()).unwrap();
    std::fs::write(dest.join("existing/keep.txt").as_path(), b"mine").unwrap();
    let bytes = tar_zst(&[Item::File("new.txt", b"theirs".to_vec())]);
    let artifact = artifact(tmp.path(), "prebuilt.tar.zst", &bytes);

    let error = extract(&artifact, dest.as_path()).unwrap_err();

    assert!(matches!(error, ExtractError::Occupied { .. }), "{error:?}");
    assert_eq!(
        std::fs::read(dest.join("existing/keep.txt").as_path()).unwrap(),
        b"mine"
    );
    assert!(!dest.join("new.txt").as_path().exists());
}

#[test]
fn an_empty_destination_is_replaced() {
    let (tmp, dest) = layout();
    std::fs::create_dir_all(dest.as_path()).unwrap();
    let bytes = tar_zst(&[Item::File("a.txt", b"payload".to_vec())]);
    let artifact = artifact(tmp.path(), "prebuilt.tar.zst", &bytes);

    let tree = extract(&artifact, dest.as_path()).unwrap();

    assert_eq!(tree.entries, 1);
    assert_eq!(
        std::fs::read(dest.join("a.txt").as_path()).unwrap(),
        b"payload"
    );
}

#[test]
fn the_decompressed_size_ceiling_is_enforced() {
    let (tmp, dest) = layout();
    let bytes = tar_zst(&[
        Item::File("a.bin", vec![b'a'; 800]),
        Item::File("b.bin", vec![b'b'; 800]),
    ]);
    let artifact = artifact(tmp.path(), "prebuilt.tar.zst", &bytes);

    let error = extract_with_limit(&artifact, dest.as_path(), 1024).unwrap_err();

    assert!(
        matches!(error, ExtractError::TooLarge { limit: 1024 }),
        "{error:?}"
    );
    assert!(!exists(&dest));
    assert_eq!(staging_debris(&dest), Vec::<String>::new());
}

#[test]
fn a_zip_is_not_mistaken_for_a_prebuilt_archive() {
    let (tmp, dest) = layout();
    let bytes = tar_zst(&[Item::File("a.txt", b"payload".to_vec())]);
    let artifact = artifact(tmp.path(), "prebuilt.zip", &bytes);

    let error = extract(&artifact, dest.as_path()).unwrap_err();

    assert!(
        matches!(error, ExtractError::Unsupported(ref name) if name == "prebuilt.zip"),
        "{error:?}"
    );
}
