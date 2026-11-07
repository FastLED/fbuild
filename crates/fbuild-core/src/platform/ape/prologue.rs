//! Parsing of the APE shell prologue: where an image keeps its per-CPU
//! loaders and the macOS loader source.
//!
//! cosmocc emits the same line shapes for every image; only the numbers
//! change. Recognized lines, keyed by the enclosing `if [ "$m" = <cpu> ]`:
//!
//! * `dd if="$o" skip=N count=M … | gzip -dc >"$t.$$"` — a gzip'd loader.
//!   When the next line patches it (`dd if="$t.$$" of="$t.$$" skip=5 count=8
//!   bs=64`) it is the macOS x86_64 loader (ELF blob turned Mach-O), otherwise
//!   the Linux loader for the CPU.
//! * `dd if="$o" skip=N count=M … | gzip -dc >"$t.c.$$"` — the gzip'd C
//!   source of the Apple Silicon loader (`ape-m1.c`), compiled with `cc`.

use super::super::host::HostArch;

/// Byte range `(skip, count)` within the image.
pub(crate) type Range = (u64, u64);

/// Everything fbuild needs from one image's prologue.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct Prologue {
    pub linux_loader_x86_64: Option<Range>,
    pub linux_loader_aarch64: Option<Range>,
    pub macos_loader_x86_64: Option<Range>,
    pub macos_loader_source_aarch64: Option<Range>,
}

impl Prologue {
    pub(crate) fn linux_loader(&self, arch: HostArch) -> Option<Range> {
        match arch {
            HostArch::X86_64 => self.linux_loader_x86_64,
            HostArch::Aarch64 => self.linux_loader_aarch64,
            _ => None,
        }
    }
}

/// Parse the prologue text at the start of an APE image.
pub(crate) fn parse(prologue: &[u8]) -> Prologue {
    let text = String::from_utf8_lossy(prologue);
    let mut out = Prologue::default();
    let mut cpu = None;
    let mut lines = text.lines().map(str::trim).peekable();
    while let Some(line) = lines.next() {
        if line.contains("\"$m\" = x86_64") {
            cpu = Some(HostArch::X86_64);
        } else if line.contains("\"$m\" = aarch64") {
            cpu = Some(HostArch::Aarch64);
        }
        let Some(arch) = cpu else { continue };

        if line.starts_with("dd if=\"$o\" ") && line.contains("| gzip -dc >\"$t.c.$$\"") {
            if arch == HostArch::Aarch64 {
                out.macos_loader_source_aarch64 = range(line);
            }
        } else if line.starts_with("dd if=\"$o\" ") && line.contains("| gzip -dc >\"$t.$$\"") {
            let patched = lines
                .peek()
                .is_some_and(|next| next.starts_with("dd if=\"$t.$$\" of=\"$t.$$\""));
            match (arch, patched) {
                (HostArch::X86_64, true) => out.macos_loader_x86_64 = range(line),
                (HostArch::X86_64, false) => out.linux_loader_x86_64 = range(line),
                (HostArch::Aarch64, false) => out.linux_loader_aarch64 = range(line),
                _ => {}
            }
        }
    }
    out
}

fn range(line: &str) -> Option<Range> {
    let field = |key: &str| {
        line.split_whitespace()
            .find_map(|tok| tok.strip_prefix(key))
            .and_then(|v| v.parse::<u64>().ok())
    };
    Some((field("skip=")?, field("count=")?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixture_prologue_yields_every_branch() {
        let image = std::fs::read(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("data/ape-hello/hello.com"),
        )
        .unwrap();
        let p = parse(&image[..64 * 1024]);
        assert_eq!(p.linux_loader_x86_64, Some((275392, 4180)));
        assert_eq!(p.linux_loader_aarch64, Some((279572, 4928)));
        assert_eq!(p.macos_loader_x86_64, Some((275392, 4180)));
        assert_eq!(p.macos_loader_source_aarch64, Some((284500, 10590)));
    }

    #[test]
    fn garbage_parses_to_nothing() {
        assert_eq!(parse(b"MZqFpD='\n\0\xff\xfe random\n"), Prologue::default());
    }
}
