//! Arduino-ESP8266 flash image: `eboot` + sketch, as the core's
//! `tools/elf2bin.py` builds it.
//!
//! The core links the sketch's `.irom0.text` at flash offset 0x1010, directly
//! behind a 4 KiB `eboot` image, so neither `objcopy -O binary` (a raw memory
//! dump starting with 0xFF) nor `esptool elf2image` (which splits irom at
//! 0x10000) produces something the ROM can boot. This is a byte-for-byte port
//! of `elf2bin.py`: two ESP8266 v1 images (magic 0xE9), the first padded to
//! 0x1000 with 0xAA, followed by eboot's CRC over the whole file.

use object::{Object, ObjectSection};

use fbuild_core::{FbuildError, Result};

const EBOOT_SECTIONS: &[&str] = &[".text", ".rodata"];
const APP_SECTIONS: &[&str] = &[".irom0.text", ".text", ".text1", ".data", ".rodata"];
const EBOOT_SLOT: usize = 4096;
const CRC_SIZE_OFFSET: usize = 4096 + 16;
const CRC_VALUE_OFFSET: usize = 4096 + 16 + 4;

/// Build the flashable image. `flash_mode` is `dio`/`dout`/`qio`/`qout`,
/// `flash_freq` an esptool frequency such as `40m`, `flash_size_bytes` the
/// board's flash size.
pub fn elf2bin(
    eboot_elf: &[u8],
    app_elf: &[u8],
    flash_mode: &str,
    flash_freq: &str,
    flash_size_bytes: u64,
) -> Result<Vec<u8>> {
    let mode = match flash_mode {
        "qio" => 0,
        "qout" => 1,
        "dio" => 2,
        "dout" => 3,
        other => return Err(err(format!("unsupported ESP8266 flash mode `{other}`"))),
    };
    let freq = match flash_freq.trim_end_matches(['m', 'M']) {
        "40" => 0,
        "26" => 1,
        "20" => 2,
        "80" => 15,
        other => {
            return Err(err(format!(
                "unsupported ESP8266 flash frequency `{other}`"
            )));
        }
    };
    let size = match flash_size_bytes {
        0x4_0000 => 1,
        0x8_0000 => 0,
        0x10_0000 => 2,
        0x20_0000 => 3,
        0x40_0000 => 4,
        0x80_0000 => 8,
        0x100_0000 => 9,
        other => return Err(err(format!("unsupported ESP8266 flash size {other} bytes"))),
    };
    let flags = [mode, freq + 16 * size];

    let mut out = Vec::new();
    write_image(
        &mut out,
        eboot_elf,
        "eboot",
        EBOOT_SECTIONS,
        flags,
        EBOOT_SLOT,
    )?;
    write_image(&mut out, app_elf, "sketch", APP_SECTIONS, flags, 0)?;
    add_crc(&mut out)?;
    Ok(out)
}

fn write_image(
    out: &mut Vec<u8>,
    elf: &[u8],
    label: &str,
    sections: &[&str],
    flags: [u8; 2],
    pad_to: usize,
) -> Result<()> {
    let file = object::File::parse(elf).map_err(|e| err(format!("{label} ELF: {e}")))?;
    let entry = u32::try_from(file.entry())
        .map_err(|_| err(format!("{label} ELF entry point does not fit 32 bits")))?;
    let start = out.len();
    out.extend([0xE9, sections.len() as u8, flags[0], flags[1]]);
    out.extend(entry.to_le_bytes());
    let mut checksum: u8 = 0xEF;
    for name in sections {
        let section = file
            .section_by_name(name)
            .ok_or_else(|| err(format!("{label} ELF has no `{name}` section")))?;
        let data = section
            .data()
            .map_err(|e| err(format!("{label} `{name}`: {e}")))?;
        let addr = u32::try_from(section.address())
            .map_err(|_| err(format!("{label} `{name}` address does not fit 32 bits")))?;
        let len =
            u32::try_from(data.len()).map_err(|_| err(format!("{label} `{name}` is too large")))?;
        out.extend(addr.to_le_bytes());
        out.extend(len.to_le_bytes());
        out.extend_from_slice(data);
        checksum = data.iter().fold(checksum, |acc, byte| acc ^ byte);
    }
    // One checksum byte ends the image, at the last byte of a 16-byte block.
    while !(out.len() - start + 1).is_multiple_of(16) {
        out.push(0);
    }
    out.push(checksum);
    if pad_to != 0 {
        let written = out.len() - start;
        if written + 8 > pad_to {
            return Err(err(format!(
                "{label} image is {} bytes, larger than its {pad_to}-byte slot",
                written + 8
            )));
        }
        out.resize(start + pad_to, 0xAA);
    }
    Ok(())
}

/// eboot verifies the image with a CRC over the whole file, stored with the
/// file length just past the eboot slot.
fn add_crc(raw: &mut [u8]) -> Result<()> {
    if raw.len() < CRC_VALUE_OFFSET + 4 {
        return Err(err("image too small for the eboot CRC fields".into()));
    }
    store_word(raw, CRC_SIZE_OFFSET, 0);
    store_word(raw, CRC_VALUE_OFFSET, 0);
    let crc = crc8266(raw);
    let len = u32::try_from(raw.len()).map_err(|_| err("image too large".into()))?;
    store_word(raw, CRC_SIZE_OFFSET, len);
    store_word(raw, CRC_VALUE_OFFSET, crc);
    Ok(())
}

fn store_word(raw: &mut [u8], offset: usize, value: u32) {
    raw[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

/// CRC-32/MPEG-2 bit order as eboot computes it (MSB first, no reflection,
/// init 0xFFFFFFFF, no final xor).
fn crc8266(data: &[u8]) -> u32 {
    let mut crc: u32 = 0xFFFF_FFFF;
    for byte in data {
        for bit in (0..8).rev() {
            let top = crc & 0x8000_0000 != 0;
            let input = byte & (1 << bit) != 0;
            crc <<= 1;
            if top != input {
                crc ^= 0x04C1_1DB7;
            }
        }
    }
    crc
}

fn err(message: String) -> FbuildError {
    FbuildError::BuildFailed(format!("ESP8266 elf2bin: {message}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use object::write::{Object as WriteObject, Symbol, SymbolSection};
    use object::{
        Architecture, BinaryFormat, Endianness, SectionKind, SymbolFlags, SymbolKind, SymbolScope,
    };

    /// A minimal 32-bit little-endian ELF with the given sections at fixed
    /// addresses and an entry point.
    fn elf(sections: &[(&str, u64, &[u8])], entry: u64) -> Vec<u8> {
        let mut obj = WriteObject::new(BinaryFormat::Elf, Architecture::Xtensa, Endianness::Little);
        for (name, addr, data) in sections {
            let id = obj.add_section(Vec::new(), name.as_bytes().to_vec(), SectionKind::Text);
            obj.append_section_data(id, data, 4);
            obj.add_symbol(Symbol {
                name: format!("sym{addr}").into_bytes(),
                value: 0,
                size: 0,
                kind: SymbolKind::Text,
                scope: SymbolScope::Linkage,
                weak: false,
                section: SymbolSection::Section(id),
                flags: SymbolFlags::None,
            });
        }
        let bytes = obj.write().unwrap();
        patch_elf32(bytes, sections, entry)
    }

    /// `object::write` emits relocatable objects (addresses 0, no entry), so
    /// set `e_entry` and each section's `sh_addr` to model a linked image.
    fn patch_elf32(mut bytes: Vec<u8>, sections: &[(&str, u64, &[u8])], entry: u64) -> Vec<u8> {
        let rd = |b: &[u8], o: usize, n: usize| -> u64 {
            b[o..o + n]
                .iter()
                .rev()
                .fold(0, |acc, x| (acc << 8) | u64::from(*x))
        };
        bytes[24..28].copy_from_slice(&(entry as u32).to_le_bytes());
        let shoff = rd(&bytes, 32, 4) as usize;
        let shentsize = rd(&bytes, 46, 2) as usize;
        let shnum = rd(&bytes, 48, 2) as usize;
        let shstrndx = rd(&bytes, 50, 2) as usize;
        let strtab_off = rd(&bytes, shoff + shstrndx * shentsize + 16, 4) as usize;
        for i in 0..shnum {
            let hdr = shoff + i * shentsize;
            let name_off = strtab_off + rd(&bytes, hdr, 4) as usize;
            let end = bytes[name_off..].iter().position(|b| *b == 0).unwrap();
            let name = std::str::from_utf8(&bytes[name_off..name_off + end])
                .unwrap()
                .to_string();
            if let Some((_, addr, _)) = sections.iter().find(|(n, _, _)| *n == name) {
                bytes[hdr + 12..hdr + 16].copy_from_slice(&(*addr as u32).to_le_bytes());
            }
        }
        bytes
    }

    fn eboot() -> Vec<u8> {
        elf(
            &[
                (".text", 0x4010_f000, &[1, 2, 3, 4]),
                (".rodata", 0x3ffe_8000, &[5, 6, 7, 8]),
            ],
            0x4010_f000,
        )
    }

    fn app() -> Vec<u8> {
        elf(
            &[
                (".irom0.text", 0x4020_1010, &[0x10; 8]),
                (".text", 0x4010_0000, &[0x20; 4]),
                (".text1", 0x4010_0004, &[0x30; 4]),
                (".data", 0x3ffe_8000, &[0x40; 4]),
                (".rodata", 0x3ffe_8004, &[0x50; 4]),
            ],
            0x4010_0000,
        )
    }

    #[test]
    fn image_layout_matches_elf2bin_py() {
        let image = elf2bin(&eboot(), &app(), "dio", "40m", 4 * 1024 * 1024).unwrap();

        // eboot: magic, 2 segments, dio, 40 MHz + 16 * 4M, entry.
        assert_eq!(&image[..8], &[0xE9, 2, 2, 0x40, 0x00, 0xF0, 0x10, 0x40]);
        // eboot segment 1 header + data.
        assert_eq!(
            &image[8..20],
            &[0x00, 0xF0, 0x10, 0x40, 4, 0, 0, 0, 1, 2, 3, 4]
        );
        // eboot slot padded with 0xAA up to the sketch image at 0x1000.
        assert_eq!(image[EBOOT_SLOT - 1], 0xAA);
        assert_eq!(&image[EBOOT_SLOT..EBOOT_SLOT + 4], &[0xE9, 5, 2, 0x40]);
        // First sketch segment is irom at 0x40201010, i.e. flash 0x1010.
        assert_eq!(
            &image[EBOOT_SLOT + 8..EBOOT_SLOT + 12],
            &0x4020_1010u32.to_le_bytes()
        );
        // Each image ends 16-byte aligned with its XOR checksum.
        assert_eq!(image.len() % 16, 0);
        let app_checksum = [0x10u8; 8]
            .iter()
            .chain(&[0x20; 4])
            .chain(&[0x30; 4])
            .chain(&[0x40; 4])
            .chain(&[0x50; 4])
            .fold(0xEFu8, |a, b| a ^ b);
        assert_eq!(*image.last().unwrap(), app_checksum);
    }

    #[test]
    fn crc_fields_hold_length_and_crc_of_the_zeroed_file() {
        let image = elf2bin(&eboot(), &app(), "dio", "40m", 4 * 1024 * 1024).unwrap();
        let word = |o: usize| u32::from_le_bytes(image[o..o + 4].try_into().unwrap());
        assert_eq!(word(CRC_SIZE_OFFSET) as usize, image.len());
        let mut zeroed = image.clone();
        store_word(&mut zeroed, CRC_SIZE_OFFSET, 0);
        store_word(&mut zeroed, CRC_VALUE_OFFSET, 0);
        assert_eq!(word(CRC_VALUE_OFFSET), crc8266(&zeroed));
    }

    #[test]
    fn crc8266_matches_crc32_mpeg2() {
        // CRC-32/MPEG-2 check value for "123456789".
        assert_eq!(crc8266(b"123456789"), 0x0376_E6E7);
    }

    #[test]
    fn missing_section_and_bad_params_are_errors() {
        let no_text1 = elf(&[(".irom0.text", 0x4020_1010, &[0; 4])], 0x4010_0000);
        assert!(elf2bin(&eboot(), &no_text1, "dio", "40m", 4 << 20).is_err());
        assert!(elf2bin(&eboot(), &app(), "fast", "40m", 4 << 20).is_err());
        assert!(elf2bin(&eboot(), &app(), "dio", "60m", 4 << 20).is_err());
        assert!(elf2bin(&eboot(), &app(), "dio", "40m", 3 << 20).is_err());
    }
}
