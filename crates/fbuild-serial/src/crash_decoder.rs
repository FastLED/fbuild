//! ESP32 crash stack trace decoder.
//!
//! Intercepts crash output from the serial monitor, extracts memory addresses,
//! runs `addr2line` against the firmware ELF, and returns decoded function names
//! and source locations.
//!
//! Supports:
//! - RISC-V (ESP32-C6, ESP32-C3, ESP32-H2): MEPC/RA register dumps
//! - Xtensa (ESP32, ESP32-S2, ESP32-S3): Backtrace lines
//! - Stack memory pointer dumps (both architectures)

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use regex::Regex;
use tracing;

use fbuild_core::subprocess::run_command;

// --- Crash detection patterns ---

const CRASH_START_PATTERNS: &[&str] = &[
    "Guru Meditation Error",
    "panic'ed",
    "Core  0 register dump",
    "Core  1 register dump",
    "LoadProhibited",
    "StoreProhibited",
    "Unhandled exception",
    "abort() was called",
    "Task watchdog got triggered",
];

/// Debounce: skip identical crash dumps within this window.
const DEBOUNCE_SECONDS: f64 = 10.0;

/// Timeout for addr2line subprocess.
const ADDR2LINE_TIMEOUT: Duration = Duration::from_secs(5);

// --- Lazily-compiled regexes ---

fn riscv_register_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?:MEPC|RA|SP|GP|TP|T[0-6]|S[0-9]|S1[01]|A[0-7])\s*:\s*(0x[0-9a-fA-F]+)")
            .unwrap()
    })
}

fn xtensa_backtrace_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"Backtrace:\s*((?:0x[0-9a-fA-F]+:0x[0-9a-fA-F]+\s*)+)").unwrap())
}

fn xtensa_addr_pair_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(0x[0-9a-fA-F]+):0x[0-9a-fA-F]+").unwrap())
}

fn stack_pointer_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"0x(?:3[CcFf]|4[02]|50)[0-9a-fA-F]{6}").unwrap())
}

fn abort_pc_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"abort\(\) was called at PC (0x[0-9a-fA-F]+)").unwrap())
}

// --- CrashDecoder ---

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DecoderState {
    Idle,
    Accumulating,
}

/// Accumulates ESP32 crash dump lines and decodes them with addr2line.
///
/// The decoder is a state machine that processes serial lines one at a time.
/// When a crash dump completes, it extracts addresses, runs `addr2line`, and
/// returns decoded output lines.
pub struct CrashDecoder {
    elf_path: Option<PathBuf>,
    addr2line_path: Option<PathBuf>,
    state: DecoderState,
    buffer: Vec<String>,
    blank_line_count: u32,
    last_crash_hash: Option<u64>,
    last_crash_time: Option<Instant>,
    warned_no_elf: bool,
    warned_no_addr2line: bool,
}

impl CrashDecoder {
    /// Create a new crash decoder.
    ///
    /// Both paths may be `None` — the decoder will gracefully degrade,
    /// warning once that decoding is disabled.
    pub fn new(elf_path: Option<PathBuf>, addr2line_path: Option<PathBuf>) -> Self {
        Self {
            elf_path,
            addr2line_path,
            state: DecoderState::Idle,
            buffer: Vec::new(),
            blank_line_count: 0,
            last_crash_hash: None,
            last_crash_time: None,
            warned_no_elf: false,
            warned_no_addr2line: false,
        }
    }

    /// Whether both ELF and addr2line are available for decoding.
    pub fn can_decode(&self) -> bool {
        self.elf_path.is_some() && self.addr2line_path.is_some()
    }

    /// Whether the decoder is currently buffering crash lines.
    pub fn is_accumulating(&self) -> bool {
        self.state == DecoderState::Accumulating
    }

    /// Process a single serial line through the crash decoder state machine.
    ///
    /// Returns `Some(lines)` when a crash dump has been fully decoded,
    /// or `None` if no output is ready yet.
    pub fn process_line(&mut self, line: &str) -> Option<Vec<String>> {
        match self.state {
            DecoderState::Idle => {
                if Self::detect_crash_start(line) {
                    self.accumulate(line);
                }
                None
            }
            DecoderState::Accumulating => {
                if self.detect_crash_end(line) {
                    let decoded = self.decode();
                    self.reset();
                    if decoded.is_empty() {
                        None
                    } else {
                        Some(decoded)
                    }
                } else {
                    self.accumulate(line);
                    None
                }
            }
        }
    }

    /// Check if a serial line indicates the start of a crash dump.
    fn detect_crash_start(line: &str) -> bool {
        CRASH_START_PATTERNS
            .iter()
            .any(|pattern| line.contains(pattern))
    }

    /// Buffer a line that is part of an active crash dump.
    fn accumulate(&mut self, line: &str) {
        self.state = DecoderState::Accumulating;
        self.buffer.push(line.to_string());
        if !line.trim().is_empty() {
            self.blank_line_count = 0;
        }
    }

    /// Check if a crash dump has ended.
    fn detect_crash_end(&mut self, line: &str) -> bool {
        // Explicit end markers
        if line.contains("ELF file SHA256") || line.contains("Rebooting...") {
            self.buffer.push(line.to_string());
            return true;
        }

        // Two consecutive blank lines signal end of dump
        if line.trim().is_empty() {
            self.blank_line_count += 1;
            return self.blank_line_count >= 2;
        }

        self.blank_line_count = 0;
        false
    }

    /// Decode the buffered crash dump using addr2line.
    fn decode(&mut self) -> Vec<String> {
        if self.buffer.is_empty() {
            return Vec::new();
        }

        // Check prerequisites before debounce — no point tracking
        // duplicates if we can't decode anyway.
        let Some(elf_path) = &self.elf_path else {
            if !self.warned_no_elf {
                self.warned_no_elf = true;
                return vec!["  [crash decode disabled — no firmware.elf found]".to_string()];
            }
            return Vec::new();
        };
        let elf_path = elf_path.clone();

        let Some(addr2line_path) = &self.addr2line_path else {
            if !self.warned_no_addr2line {
                self.warned_no_addr2line = true;
                return vec!["  [crash decode disabled — addr2line not found]".to_string()];
            }
            return Vec::new();
        };
        let addr2line_path = addr2line_path.clone();

        // Debounce: skip if identical crash within the window
        if self.is_duplicate_crash() {
            return vec!["  [crash decode skipped — duplicate within debounce window]".to_string()];
        }

        // Extract addresses
        let addresses = self.extract_addresses();
        if addresses.is_empty() {
            return Vec::new();
        }

        // Run addr2line
        Self::run_addr2line(&addr2line_path, &elf_path, &addresses)
    }

    /// Clear the accumulator for the next crash.
    fn reset(&mut self) {
        self.buffer.clear();
        self.state = DecoderState::Idle;
        self.blank_line_count = 0;
    }

    /// Check and update debounce state. Returns true if this is a duplicate.
    fn is_duplicate_crash(&mut self) -> bool {
        let mut hasher = DefaultHasher::new();
        self.buffer.hash(&mut hasher);
        let crash_hash = hasher.finish();
        let now = Instant::now();

        let is_dup = if let (Some(prev_hash), Some(prev_time)) =
            (self.last_crash_hash, self.last_crash_time)
        {
            prev_hash == crash_hash
                && now.duration_since(prev_time).as_secs_f64() < DEBOUNCE_SECONDS
        } else {
            false
        };

        self.last_crash_hash = Some(crash_hash);
        self.last_crash_time = Some(now);

        is_dup
    }

    /// Extract unique code addresses from the buffered crash dump.
    fn extract_addresses(&self) -> Vec<String> {
        let full_text = self.buffer.join("\n");
        let mut seen = std::collections::HashSet::new();
        let mut addresses = Vec::new();

        let mut add = |addr: &str| {
            let low = addr.to_lowercase();
            if seen.insert(low) {
                addresses.push(addr.to_string());
            }
        };

        // "abort() was called at PC 0x..."
        for m in abort_pc_re().captures_iter(&full_text) {
            if let Some(cap) = m.get(1) {
                add(cap.as_str());
            }
        }

        // Xtensa backtrace (PC:SP pairs — extract PC)
        for bt_match in xtensa_backtrace_re().captures_iter(&full_text) {
            if let Some(bt_group) = bt_match.get(1) {
                for pair_match in xtensa_addr_pair_re().captures_iter(bt_group.as_str()) {
                    if let Some(pc) = pair_match.get(1) {
                        add(pc.as_str());
                    }
                }
            }
        }

        // RISC-V named registers — only code regions (0x40, 0x42)
        for m in riscv_register_re().captures_iter(&full_text) {
            if let Some(cap) = m.get(1) {
                let val = cap.as_str();
                let low = val.to_lowercase();
                if low.starts_with("0x40") || low.starts_with("0x42") {
                    add(val);
                }
            }
        }

        // Stack memory pointers — only code regions
        for m in stack_pointer_re().find_iter(&full_text) {
            let val = m.as_str();
            let low = val.to_lowercase();
            if low.starts_with("0x40") || low.starts_with("0x42") {
                add(val);
            }
        }

        addresses
    }

    /// Run addr2line on the extracted addresses.
    fn run_addr2line(addr2line_path: &Path, elf_path: &Path, addresses: &[String]) -> Vec<String> {
        let addr2line_str = addr2line_path.to_string_lossy();
        let elf_str = elf_path.to_string_lossy();

        let mut args: Vec<&str> = vec![&addr2line_str, "-pfiaC", "-e", &elf_str];
        for addr in addresses {
            args.push(addr.as_str());
        }

        let result = match run_command(&args, None, None, Some(ADDR2LINE_TIMEOUT)) {
            Ok(output) => output,
            Err(fbuild_core::FbuildError::Timeout(_)) => {
                tracing::warn!("addr2line timed out after {}s", ADDR2LINE_TIMEOUT.as_secs());
                return vec!["  [crash decode timed out]".to_string()];
            }
            Err(e) => {
                tracing::warn!("addr2line failed: {}", e);
                return vec![format!("  [crash decode error: {e}]")];
            }
        };

        if !result.success() {
            let stderr = result.stderr.trim();
            tracing::warn!("addr2line returned {}: {}", result.exit_code, stderr);
            return vec![format!("  [addr2line error: {stderr}]")];
        }

        // Format output
        let mut output_lines = vec![String::new(), "=== Decoded Stack Trace ===".to_string()];
        for raw_line in result.stdout.trim().lines() {
            let stripped = raw_line.trim();
            if !stripped.is_empty() && stripped != "??:0" && stripped != "?? ??:0" {
                output_lines.push(format!("  {stripped}"));
            }
        }
        output_lines.push("===========================".to_string());
        output_lines.push(String::new());

        // Only return if we got something useful (header + footer + 2 blanks = 4)
        if output_lines.len() <= 4 {
            return Vec::new();
        }

        output_lines
    }
}

/// Derive addr2line path from the compiler (gcc) path.
///
/// The toolchain prefix is everything before `gcc` in the binary name:
/// - `riscv32-esp-elf-gcc` → `riscv32-esp-elf-addr2line`
/// - `xtensa-esp32s3-elf-gcc` → `xtensa-esp32s3-elf-addr2line`
pub fn derive_addr2line_path(cc_path: &Path) -> Option<PathBuf> {
    let name = cc_path.file_name()?.to_string_lossy();

    // Strip .exe suffix for matching
    let stem = name.replace(".exe", "");
    if !stem.ends_with("gcc") {
        return None;
    }

    let prefix = &stem[..stem.len() - 3]; // e.g. "riscv32-esp-elf-"
    let suffix = if cfg!(windows) { ".exe" } else { "" };
    let addr2line = cc_path.parent()?.join(format!("{prefix}addr2line{suffix}"));

    if addr2line.exists() {
        Some(addr2line)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- Crash start detection ---

    #[test]
    fn detect_crash_start_guru_meditation() {
        assert!(CrashDecoder::detect_crash_start(
            "Guru Meditation Error: Core  0 panic'ed (LoadProhibited)"
        ));
    }

    #[test]
    fn detect_crash_start_abort() {
        assert!(CrashDecoder::detect_crash_start(
            "abort() was called at PC 0x42002a3c on core 0"
        ));
    }

    #[test]
    fn detect_crash_start_watchdog() {
        assert!(CrashDecoder::detect_crash_start(
            "Task watchdog got triggered. The following tasks did not reset the watchdog in time:"
        ));
    }

    #[test]
    fn detect_crash_start_normal_line() {
        assert!(!CrashDecoder::detect_crash_start("Hello from ESP32!"));
    }

    #[test]
    fn detect_crash_start_all_patterns() {
        for pattern in CRASH_START_PATTERNS {
            assert!(
                CrashDecoder::detect_crash_start(&format!("prefix {pattern} suffix")),
                "failed to detect pattern: {pattern}"
            );
        }
    }

    // --- Crash end detection ---

    #[test]
    fn detect_crash_end_elf_sha256() {
        let mut decoder = CrashDecoder::new(None, None);
        decoder.state = DecoderState::Accumulating;
        assert!(decoder.detect_crash_end("ELF file SHA256:  0xdeadbeef"));
    }

    #[test]
    fn detect_crash_end_rebooting() {
        let mut decoder = CrashDecoder::new(None, None);
        decoder.state = DecoderState::Accumulating;
        assert!(decoder.detect_crash_end("Rebooting..."));
    }

    #[test]
    fn detect_crash_end_two_blank_lines() {
        let mut decoder = CrashDecoder::new(None, None);
        decoder.state = DecoderState::Accumulating;
        assert!(!decoder.detect_crash_end(""));
        assert!(decoder.detect_crash_end(""));
    }

    #[test]
    fn detect_crash_end_blank_then_content_resets() {
        let mut decoder = CrashDecoder::new(None, None);
        decoder.state = DecoderState::Accumulating;
        assert!(!decoder.detect_crash_end(""));
        assert!(!decoder.detect_crash_end("some content"));
        // Counter was reset, need two more blanks
        assert!(!decoder.detect_crash_end(""));
        assert!(decoder.detect_crash_end(""));
    }

    // --- Address extraction ---

    #[test]
    fn extract_xtensa_backtrace() {
        let mut decoder = CrashDecoder::new(None, None);
        decoder.buffer = vec![
            "Guru Meditation Error: Core  0 panic'ed (LoadProhibited)".to_string(),
            "Backtrace: 0x40081234:0x3ffb0000 0x42002a3c:0x3ffb0010 0x40085678:0x3ffb0020"
                .to_string(),
        ];
        let addrs = decoder.extract_addresses();
        assert_eq!(addrs, vec!["0x40081234", "0x42002a3c", "0x40085678"]);
    }

    #[test]
    fn extract_riscv_registers() {
        let mut decoder = CrashDecoder::new(None, None);
        decoder.buffer = vec![
            "Core  0 register dump:".to_string(),
            "MEPC    : 0x42001234  RA      : 0x42005678  SP      : 0x3fc90000".to_string(),
            "GP      : 0x3fc80000  TP      : 0x3fc70000  T0      : 0x00000001".to_string(),
        ];
        let addrs = decoder.extract_addresses();
        // Only code regions (0x40, 0x42) are kept
        assert_eq!(addrs, vec!["0x42001234", "0x42005678"]);
    }

    #[test]
    fn extract_abort_pc() {
        let mut decoder = CrashDecoder::new(None, None);
        decoder.buffer = vec![
            "abort() was called at PC 0x42002a3c on core 0".to_string(),
            "Backtrace: 0x40381d5a:0x3fc90000".to_string(),
        ];
        let addrs = decoder.extract_addresses();
        assert_eq!(addrs, vec!["0x42002a3c", "0x40381d5a"]);
    }

    #[test]
    fn extract_stack_pointers() {
        let mut decoder = CrashDecoder::new(None, None);
        decoder.buffer = vec!["3fc90000: 0x42001234 0x00000000 0x3fc80000 0x40085678".to_string()];
        let addrs = decoder.extract_addresses();
        assert_eq!(addrs, vec!["0x42001234", "0x40085678"]);
    }

    #[test]
    fn extract_deduplicates_addresses() {
        let mut decoder = CrashDecoder::new(None, None);
        decoder.buffer = vec![
            "abort() was called at PC 0x42002a3c on core 0".to_string(),
            "Backtrace: 0x42002a3c:0x3fc90000 0x40081234:0x3fc90010".to_string(),
        ];
        let addrs = decoder.extract_addresses();
        // 0x42002a3c appears in both abort and backtrace, should only appear once
        assert_eq!(addrs, vec!["0x42002a3c", "0x40081234"]);
    }

    // --- process_line state machine ---

    #[test]
    fn process_line_normal_line() {
        let mut decoder = CrashDecoder::new(None, None);
        assert!(decoder.process_line("Hello from ESP32!").is_none());
        assert!(!decoder.is_accumulating());
    }

    #[test]
    fn process_line_crash_start_begins_accumulating() {
        let mut decoder = CrashDecoder::new(None, None);
        assert!(decoder
            .process_line("Guru Meditation Error: Core  0 panic'ed (LoadProhibited)")
            .is_none());
        assert!(decoder.is_accumulating());
    }

    #[test]
    fn process_line_accumulates_then_ends() {
        let mut decoder = CrashDecoder::new(None, None);

        // Start
        decoder.process_line("Guru Meditation Error: Core  0 panic'ed (LoadProhibited)");
        assert!(decoder.is_accumulating());

        // Middle lines
        decoder.process_line("Core  0 register dump:");
        decoder.process_line("Backtrace: 0x42002a3c:0x3fc90000");

        // End — should produce output (warning about no elf)
        let result = decoder.process_line("Rebooting...");
        assert!(result.is_some());
        assert!(!decoder.is_accumulating());
    }

    #[test]
    fn process_line_no_elf_warns_once() {
        let mut decoder = CrashDecoder::new(None, None);

        // First crash — should warn
        decoder.process_line("abort() was called at PC 0x42002a3c");
        let result = decoder.process_line("Rebooting...");
        let lines = result.unwrap();
        assert!(lines[0].contains("no firmware.elf found"));

        // Second crash — should not produce output
        decoder.process_line("abort() was called at PC 0x42002a3c");
        let result = decoder.process_line("Rebooting...");
        assert!(result.is_none());
    }

    // --- Duplicate debouncing ---

    #[test]
    fn debounce_identical_crash() {
        let elf = PathBuf::from("/nonexistent/firmware.elf");
        let a2l = PathBuf::from("/nonexistent/addr2line");
        let mut decoder = CrashDecoder::new(Some(elf), Some(a2l));

        // First crash — will fail to run addr2line but will set the hash
        decoder.process_line("abort() was called at PC 0x42002a3c");
        let _result1 = decoder.process_line("Rebooting...");

        // Identical crash immediately after — should be debounced
        decoder.process_line("abort() was called at PC 0x42002a3c");
        let result2 = decoder.process_line("Rebooting...");
        let lines = result2.unwrap();
        assert!(lines[0].contains("duplicate within debounce window"));
    }

    // --- derive_addr2line_path ---

    #[test]
    fn derive_addr2line_from_gcc() {
        // We can't test with real files, but we can test the logic
        // by checking that a non-existent path returns None
        let gcc = PathBuf::from("/tmp/xtensa-esp-elf-gcc");
        assert!(derive_addr2line_path(&gcc).is_none()); // file doesn't exist

        let not_gcc = PathBuf::from("/tmp/xtensa-esp-elf-clang");
        assert!(derive_addr2line_path(&not_gcc).is_none());
    }

    // --- Regex pattern tests ---

    #[test]
    fn regex_riscv_register() {
        let re = riscv_register_re();
        let line = "MEPC    : 0x42001234  RA      : 0x42005678  SP      : 0x3fc90000";
        let addrs: Vec<&str> = re
            .captures_iter(line)
            .map(|c| c.get(1).unwrap().as_str())
            .collect();
        assert_eq!(addrs, vec!["0x42001234", "0x42005678", "0x3fc90000"]);
    }

    #[test]
    fn regex_xtensa_backtrace() {
        let re = xtensa_backtrace_re();
        let line = "Backtrace: 0x40081234:0x3ffb0000 0x42002a3c:0x3ffb0010";
        let caps = re.captures(line).unwrap();
        let pairs = caps.get(1).unwrap().as_str();

        let pair_re = xtensa_addr_pair_re();
        let pcs: Vec<&str> = pair_re
            .captures_iter(pairs)
            .map(|c| c.get(1).unwrap().as_str())
            .collect();
        assert_eq!(pcs, vec!["0x40081234", "0x42002a3c"]);
    }

    #[test]
    fn regex_stack_pointer() {
        let re = stack_pointer_re();
        let line = "3fc90000: 0x42001234 0x00000000 0x3fc80000 0x40085678";
        let addrs: Vec<&str> = re.find_iter(line).map(|m| m.as_str()).collect();
        assert_eq!(addrs, vec!["0x42001234", "0x3fc80000", "0x40085678"]);
    }

    #[test]
    fn regex_abort_pc() {
        let re = abort_pc_re();
        let line = "abort() was called at PC 0x42002a3c on core 0";
        let caps = re.captures(line).unwrap();
        assert_eq!(caps.get(1).unwrap().as_str(), "0x42002a3c");
    }

    // --- Full Xtensa crash dump ---

    #[test]
    fn full_xtensa_crash_dump() {
        let mut decoder = CrashDecoder::new(None, None);

        let lines = [
            "Guru Meditation Error: Core  0 panic'ed (LoadProhibited). Exception was unhandled.",
            "Core  0 register dump:",
            "PC      : 0x400d1234  PS      : 0x00060030  A0      : 0x800d5678",
            "A1      : 0x3ffb0000  A2      : 0x00000000  A3      : 0x3ffb0010",
            "",
            "Backtrace: 0x400d1234:0x3ffb0000 0x400d5678:0x3ffb0010 0x400d9abc:0x3ffb0020",
            "",
            "ELF file SHA256:  0123456789abcdef",
        ];

        let mut result = None;
        for line in &lines {
            if let Some(r) = decoder.process_line(line) {
                result = Some(r);
            }
        }

        // Should produce a warning about no elf
        let output = result.unwrap();
        assert!(output[0].contains("no firmware.elf found"));
    }

    // --- Full RISC-V crash dump ---

    #[test]
    fn full_riscv_crash_dump() {
        let mut decoder = CrashDecoder::new(None, None);

        let lines = [
            "abort() was called at PC 0x42002a3c on core 0",
            "",
            "Core  0 register dump:",
            "MEPC    : 0x42002a3c  RA      : 0x42005678  SP      : 0x3fc90000",
            "GP      : 0x3fc80000  TP      : 0x3fc70000  T0      : 0x00000001",
            "T1      : 0x00000002  T2      : 0x00000003  S0      : 0x3fc60000",
            "S1      : 0x3fc50000  A0      : 0x00000000  A1      : 0x42003456",
            "",
            "Rebooting...",
        ];

        let mut result = None;
        for line in &lines {
            if let Some(r) = decoder.process_line(line) {
                result = Some(r);
            }
        }

        let output = result.unwrap();
        assert!(output[0].contains("no firmware.elf found"));
    }

    // --- Real ESP32-S3 crash decode (requires hardware build artifacts) ---

    /// Integration test: feed a real ESP32-S3 Xtensa crash dump through the
    /// decoder with a real ELF + addr2line, verify we get `deliberate_crash`
    /// in the decoded output — matching the Python fbuild decoder.
    #[test]
    #[ignore] // requires build artifacts from esp32s3-crash-test
    fn real_esp32s3_crash_decode() {
        let elf = PathBuf::from(std::env::var("ESP32S3_CRASH_ELF").unwrap_or_else(|_| {
            format!(
                "{}/.pio/build/esp32s3-crash-test/firmware.elf",
                std::env::var("TEMP")
                    .or_else(|_| std::env::var("TMP"))
                    .unwrap_or_else(|_| "/tmp".to_string())
            )
            .replace('\\', "/")
        }));
        let addr2line = PathBuf::from(std::env::var("ESP32S3_ADDR2LINE").unwrap_or_else(|_| {
            let home = std::env::var("USERPROFILE")
                .or_else(|_| std::env::var("HOME"))
                .unwrap_or_default();
            format!(
                "{}/.platformio/packages/toolchain-xtensa-esp-elf/bin/xtensa-esp-elf-addr2line{}",
                home,
                if cfg!(windows) { ".exe" } else { "" }
            )
            .replace('\\', "/")
        }));

        if !elf.exists() {
            eprintln!("Skipping: ELF not found at {:?}", elf);
            return;
        }
        if !addr2line.exists() {
            eprintln!("Skipping: addr2line not found at {:?}", addr2line);
            return;
        }

        let mut decoder = CrashDecoder::new(Some(elf), Some(addr2line));
        assert!(decoder.can_decode());

        // Real crash output captured from ESP32-S3 running the crash test sketch
        let crash_lines = [
            "FBUILD_CRASH_DECODE_TEST_START",
            "ESP32-S3 Crash Decode Test",
            "About to call abort()...",
            "",
            "abort() was called at PC 0x42001ea3 on core 1",
            "",
            "",
            "Backtrace: 0x4037b059:0x3fcebc20 0x4037b021:0x3fcebc40 0x40381d0d:0x3fcebc60 0x42001ea3:0x3fcebce0 0x42001ead:0x3fcebd00 0x42001ee3:0x3fcebd20 0x42004dd2:0x3fcebd40 0x4037bf6d:0x3fcebd60",
            "",
            "",
            "",
            "ELF file SHA256: b74eaf538",
            "",
            "Rebooting...",
        ];

        let mut decoded_output = None;
        for line in &crash_lines {
            if let Some(output) = decoder.process_line(line) {
                decoded_output = Some(output);
            }
        }

        let output = decoded_output.expect("decoder should produce output for crash dump");
        let joined = output.join("\n");
        println!("Rust decoder output:\n{}", joined);

        // Verify key expectations (matching Python decoder):
        // 1. Contains the decoded stack trace header
        assert!(
            joined.contains("=== Decoded Stack Trace ==="),
            "missing stack trace header"
        );
        // 2. Contains deliberate_crash function name
        assert!(
            joined.contains("deliberate_crash"),
            "missing deliberate_crash in decoded output:\n{joined}"
        );
        // 3. Contains main.ino source reference
        assert!(
            joined.contains("main.ino"),
            "missing main.ino source reference:\n{joined}"
        );
    }
}
