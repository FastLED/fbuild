//! Unit tests for the RP2040 *region* UF2 path — the filesystem half of
//! `fbuild uploadfs` (FastLED/fbuild#1358). No hardware is involved: every
//! assertion is about the bytes the ROM would receive.

use super::*;

fn block_field(block: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(block[offset..offset + 4].try_into().unwrap())
}

/// The first block must address the region itself, so the ROM programs the
/// bytes where the firmware's `FS_START` mount expects them — not at
/// `0x10000000`, which would overwrite the boot loader and firmware.
#[test]
fn region_uf2_starts_at_the_declared_flash_address() {
    let image = vec![0xA5u8; 700];
    let uf2 = encode_uf2_for_region(&image, 0x1017_f000).expect("region uf2");
    assert_eq!(uf2.len() % UF2_BLOCK_SIZE, 0);
    assert_eq!(
        uf2.len() / UF2_BLOCK_SIZE,
        3,
        "700 bytes is three 256-byte blocks"
    );
    let first = &uf2[..UF2_BLOCK_SIZE];
    assert_eq!(block_field(first, 12), 0x1017_f000);
    assert_eq!(block_field(first, 28), RP2040_FAMILY_ID);
    assert_eq!(
        block_field(first, 24),
        3,
        "block count covers the whole image"
    );
    // Payload of block 0 is the image's first 256 bytes.
    assert!(first[32..32 + 256].iter().all(|byte| *byte == 0xA5));
}

/// The padded tail keeps the image inside its region: the last block addresses
/// the final 256-byte window, never an address past the image.
#[test]
fn region_uf2_addresses_are_contiguous_and_inside_the_region() {
    let image = vec![0x5Au8; 300];
    let uf2 = encode_uf2_for_region(&image, 0x1017_f000).expect("region uf2");
    let addresses: Vec<u32> = uf2
        .chunks_exact(UF2_BLOCK_SIZE)
        .map(|block| block_field(block, 12))
        .collect();
    assert_eq!(addresses, vec![0x1017_f000, 0x1017_f100]);
}

/// An empty image has nothing to flash; refusing beats writing zero blocks the
/// ROM would ignore while the command reports success.
#[test]
fn region_uf2_rejects_an_empty_image() {
    let error = encode_uf2_for_region(&[], 0x1017_f000)
        .expect_err("empty image")
        .to_string();
    assert!(error.contains("empty region image"), "{error}");
}

/// The ROM rejects unaligned block targets, so an unaligned `fs_start` is
/// caught here instead of arriving as a silent no-op flash.
#[test]
fn region_uf2_rejects_an_unaligned_address() {
    let error = encode_uf2_for_region(&[0u8; 256], 0x1017_f008)
        .expect_err("unaligned address")
        .to_string();
    assert!(error.contains("not 256-byte aligned"), "{error}");
}

/// A flash address that does not fit a 32-bit UF2 field cannot be encoded.
#[test]
fn region_uf2_rejects_a_64_bit_only_address() {
    let error = encode_uf2_for_region(&[0u8; 256], 0x1_0000_0000)
        .expect_err("out of range address")
        .to_string();
    assert!(error.contains("32-bit flash address"), "{error}");
}

/// The write path puts the encoded bytes in `NEW.UF2` verbatim, which is what
/// the ROM reads.
#[test]
fn write_uf2_to_volume_writes_new_uf2() {
    let root = tempfile::tempdir().unwrap();
    fs::write(
        root.path().join("INFO_UF2.TXT"),
        "Model: Raspberry Pi RP2\n",
    )
    .unwrap();
    let image = vec![0x11u8; 600];
    let uf2 = encode_uf2_for_region(&image, 0x1013_0000).expect("region uf2");

    let destination = write_uf2_to_volume(&uf2, root.path()).expect("write");
    assert_eq!(destination, root.path().join("NEW.UF2"));
    assert_eq!(fs::read(&destination).unwrap(), uf2);
}

/// A directory without `INFO_UF2.TXT` is not a Pico volume, so it is refused
/// rather than scribbled on.
#[test]
fn write_uf2_to_volume_refuses_a_non_bootsel_directory() {
    let root = tempfile::tempdir().unwrap();
    let error = write_uf2_to_volume(&[0u8; 512], root.path())
        .expect_err("no BOOTSEL marker")
        .to_string();
    assert!(error.contains("INFO_UF2.TXT"), "{error}");
    assert!(!root.path().join("NEW.UF2").exists());
}

/// A full 512 KiB partition image — the size an arduino-pico `filesystem_size =
/// 0.5m` board produces — encodes into 2048 blocks covering exactly the region,
/// which is the image `fbuild buildfs` really writes.
#[test]
fn region_uf2_covers_a_full_half_megabyte_partition() {
    let image = vec![0x3Cu8; 512 * 1024];
    let uf2 = encode_uf2_for_region(&image, 0x1017_f000).expect("region uf2");
    assert_eq!(uf2.len() / UF2_BLOCK_SIZE, 2048);
    let addresses: Vec<u32> = uf2
        .chunks_exact(UF2_BLOCK_SIZE)
        .map(|block| block_field(block, 12))
        .collect();
    assert_eq!(addresses[0], 0x1017_f000);
    assert_eq!(
        *addresses.last().unwrap(),
        0x1017_f000 + 2047 * 256,
        "the last block addresses the final 256-byte window of the region"
    );
}
