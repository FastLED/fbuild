//! RP2040/RP2350 deployment through the stock UF2 BOOTSEL transports.

use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use fbuild_core::{FbuildError, Result};

use crate::{DeployOutcome, Deployer, DeploymentResult};

#[path = "rp2040_mount.rs"]
mod mount;
#[path = "rp2040_picotool.rs"]
mod picotool;
#[path = "rp2040_target.rs"]
mod target;
#[path = "rp2040_topology.rs"]
mod topology;
use mount::try_mount_linux_rom_device;
use target::{
    describe_unhealthy, resolve_requested_runtime_target, select_cdc_candidate, serial_selector,
};

const UF2_MAGIC_START0: u32 = 0x0A32_4655;
const UF2_MAGIC_START1: u32 = 0x9E5D_5157;
const UF2_MAGIC_END: u32 = 0x0AB1_6F30;
const UF2_FLAG_FAMILY_ID_PRESENT: u32 = 0x0000_2000;
const RP2040_FAMILY_ID: u32 = 0xE48B_FF56;
const RP2350_FAMILY_ID: u32 = 0xE48B_FF59;
// fbuild's firmware.bin is the complete flash image: it starts with the
// second-stage bootloader at the RP2040 XIP address 0x1000_0000. The 0x2000
// default used by Arduino-Pico's uf2conv.py is only for app-only BINs whose
// boot2 has already been stripped. Encoding this full image at 0x2000 leaves
// stock ROM BOOTSEL in place after an apparently successful copy.
const RP2040_UF2_BASE_ADDRESS: u32 = 0x1000_0000;
const RP2040_XIP_SRAM_START: u32 = 0x1500_0000;
const RP2040_XIP_SRAM_END: u32 = 0x1500_4000;
const RP2040_SRAM_START: u32 = 0x2000_0000;
const RP2040_SRAM_END: u32 = 0x2004_2000;
const UF2_PAYLOAD_SIZE: usize = 256;
const UF2_BLOCK_SIZE: usize = 512;
// Hub-path transients (storport timeouts, port resets) drop the BOOTSEL
// volume mid-copy and the ROM re-enumerates within seconds; a bounded number
// of fresh-enumeration retries recovers those without stalling a genuinely
// dead transport (FastLED/fbuild#1081).
const UF2_TRANSFER_ATTEMPTS: usize = 3;
// Stage-timeout env overrides (FastLED/fbuild#1082): first-plug driver
// installs and deep hub chains legitimately exceed the defaults on foreign
// machines. Values are integer seconds, accepted in 1..=600 by
// `timeout_secs_from`.
const BOOTLOADER_TIMEOUT_ENV: &str = "FBUILD_RP2040_BOOTLOADER_TIMEOUT_SECS";
const POST_DEPLOY_TIMEOUT_ENV: &str = "FBUILD_RP2040_POST_DEPLOY_TIMEOUT_SECS";
const UF2_WRITE_TIMEOUT_ENV: &str = "FBUILD_RP2040_UF2_WRITE_TIMEOUT_SECS";
const DEFAULT_UF2_WRITE_TIMEOUT: Duration = Duration::from_secs(60);
const CDC_POLL_INTERVAL: Duration = Duration::from_millis(100);
const WATCHDOG_CANCELLATION_GRACE: Duration = Duration::from_millis(250);

/// Parse an env-supplied stage timeout. Accepts integer seconds in 1..=600;
/// an unset variable is silently the default, anything else warns and falls
/// back to the default.
fn timeout_secs_from(raw: Option<&str>, default: Duration, var_name: &str) -> Duration {
    let Some(raw) = raw else {
        return default;
    };
    match raw.trim().parse::<u64>() {
        Ok(secs) if (1..=600).contains(&secs) => Duration::from_secs(secs),
        _ => {
            tracing::warn!(
                value = raw,
                var = var_name,
                default_secs = default.as_secs(),
                "ignoring invalid RP2040 timeout override; expected integer seconds in 1..=600"
            );
            default
        }
    }
}

fn timeout_from_env(var_name: &str, default: Duration) -> Duration {
    let raw = std::env::var(var_name).ok();
    timeout_secs_from(raw.as_deref(), default, var_name)
}

fn rp2040_post_deploy_timeout() -> Duration {
    let timing = fbuild_serial::boards::BoardFamily::NativeUsbCdcReset1200Bps.handoff_timing();
    timeout_from_env(
        POST_DEPLOY_TIMEOUT_ENV,
        Duration::from_millis(u64::from(timing.application_cdc_timeout_ms)),
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Uf2Target {
    Flash,
    Ram,
}

/// Build UF2 blocks for a raw RP2040 flash image.
pub fn encode_uf2(binary: &[u8]) -> Vec<u8> {
    encode_uf2_at_address(binary, RP2040_UF2_BASE_ADDRESS, RP2040_FAMILY_ID)
}

/// Build UF2 blocks using an explicit Raspberry Pi family identifier.
pub fn encode_uf2_for_family(binary: &[u8], family_id: u32) -> Vec<u8> {
    encode_uf2_at_address(binary, RP2040_UF2_BASE_ADDRESS, family_id)
}

fn encode_uf2_at_address(binary: &[u8], base_address: u32, family_id: u32) -> Vec<u8> {
    let block_count = binary.len().div_ceil(UF2_PAYLOAD_SIZE).max(1);
    let mut output = Vec::with_capacity(block_count * UF2_BLOCK_SIZE);
    for block_no in 0..block_count {
        let start = block_no * UF2_PAYLOAD_SIZE;
        let end = (start + UF2_PAYLOAD_SIZE).min(binary.len());
        let payload_len = end.saturating_sub(start);
        let mut block = [0u8; UF2_BLOCK_SIZE];
        put_u32(&mut block, 0, UF2_MAGIC_START0);
        put_u32(&mut block, 4, UF2_MAGIC_START1);
        put_u32(&mut block, 8, UF2_FLAG_FAMILY_ID_PRESENT);
        put_u32(&mut block, 12, start as u32 + base_address);
        // UF2 blocks always advertise a full 256-byte payload. The final
        // block is zero-padded; advertising its short source length makes
        // the ROM BOOTSEL parser reject an otherwise valid image.
        put_u32(&mut block, 16, UF2_PAYLOAD_SIZE as u32);
        put_u32(&mut block, 20, block_no as u32);
        put_u32(&mut block, 24, block_count as u32);
        put_u32(&mut block, 28, family_id);
        block[32..32 + payload_len].copy_from_slice(&binary[start..end]);
        put_u32(&mut block, 508, UF2_MAGIC_END);
        output.extend_from_slice(&block);
    }
    output
}

fn put_u32(buffer: &mut [u8], offset: usize, value: u32) {
    buffer[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

/// Return candidate removable roots for this host. Kept separate from the
/// marker check so unit tests can use a temporary directory on every OS.
///
/// On Windows this is filtered to `DRIVE_REMOVABLE` roots only: a
/// disconnected mapped network drive answers `fs::read_dir`/`read_to_string`
/// tens of seconds late, which would otherwise eat the whole BOOTSEL
/// discovery window before the real drive letter is reached
/// (FastLED/fbuild#1082). `find_uf2_volumes` itself stays unfiltered so
/// tests can keep passing explicit temp-dir roots.
fn volume_roots() -> Vec<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    #[cfg(windows)]
    {
        volume_roots_filtered(true, home.as_deref(), topology::is_removable_drive)
    }
    #[cfg(not(windows))]
    {
        volume_roots_filtered(false, home.as_deref(), |_: &Path| true)
    }
}

/// Unfiltered root list, kept for the pre-existing cross-platform coverage
/// test now that `volume_roots()` filters Windows letters through
/// [`topology::is_removable_drive`].
#[cfg(test)]
fn volume_roots_for(windows: bool, home: Option<&Path>) -> Vec<PathBuf> {
    volume_roots_filtered(windows, home, |_: &Path| true)
}

/// Build the default root list, applying `keep` to each Windows drive
/// letter root before it is scanned. Non-Windows roots are never filtered:
/// `keep` is only consulted in the `windows` branch.
fn volume_roots_filtered<F: Fn(&Path) -> bool>(
    windows: bool,
    home: Option<&Path>,
    keep: F,
) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if windows {
        for letter in b'A'..=b'Z' {
            let root = PathBuf::from(format!("{}:\\", letter as char));
            if keep(&root) {
                roots.push(root);
            }
        }
    } else {
        if let Some(home) = home {
            if let Some(user) = home.file_name() {
                roots.push(PathBuf::from("/media").join(user));
                roots.push(PathBuf::from("/run/media").join(user));
            }
        }
        roots.push(PathBuf::from("/Volumes"));
        roots.push(PathBuf::from("/media"));
        roots.push(PathBuf::from("/run/media"));
    }
    roots
}

fn has_uf2_marker(path: &Path) -> bool {
    let info = path.join("INFO_UF2.TXT");
    if let Ok(contents) = fs::read_to_string(info) {
        let upper = contents.to_ascii_uppercase();
        return upper.contains("RP2") || upper.contains("RP2040") || upper.contains("RP2350");
    }
    false
}

/// Find a mounted Pico BOOTSEL volume under the supplied roots.
pub fn find_uf2_volume(roots: &[PathBuf]) -> Option<PathBuf> {
    find_uf2_volumes(roots).into_iter().next()
}

fn find_uf2_volumes(roots: &[PathBuf]) -> Vec<PathBuf> {
    // Linux roots intentionally overlap (`/media/$USER` and `/media`) so
    // discovery still works when HOME is unavailable. Deduplicate paths
    // before applying the multi-board safety check.
    let mut matches = BTreeSet::new();
    for root in roots {
        if has_uf2_marker(root) {
            matches.insert(root.clone());
        }
        let Ok(entries) = fs::read_dir(root) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() && has_uf2_marker(&path) {
                matches.insert(path);
            }
        }
    }
    matches.into_iter().collect()
}

fn find_uf2_volume_until(
    timeout: Duration,
    volumes_before: &BTreeSet<PathBuf>,
) -> Result<Option<PathBuf>> {
    find_uf2_volume_until_with(
        timeout,
        || select_appeared_volume(volumes_before, find_uf2_volumes(&volume_roots())),
        try_mount_linux_rom_device,
    )
}

fn find_uf2_volume_until_with<F, M>(
    timeout: Duration,
    mut scan: F,
    mut try_mount: M,
) -> Result<Option<PathBuf>>
where
    F: FnMut() -> Result<Option<PathBuf>>,
    M: FnMut() -> bool,
{
    let deadline = Instant::now() + timeout;
    let mut mount_attempted = false;
    loop {
        if let Some(path) = scan()? {
            return Ok(Some(path));
        }
        if !mount_attempted {
            mount_attempted = try_mount();
        }
        if Instant::now() >= deadline {
            return Ok(None);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn multiple_bootsel_volumes_error(matches: &[PathBuf]) -> FbuildError {
    FbuildError::DeployFailed(format!(
        "found multiple RP2040 BOOTSEL volumes: {}; pass an explicit UF2 volume path to select one",
        matches
            .iter()
            .map(|path| path.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    ))
}

fn select_single_uf2_volume(mut matches: Vec<PathBuf>) -> Result<Option<PathBuf>> {
    if matches.len() > 1 {
        return Err(multiple_bootsel_volumes_error(&matches));
    }
    Ok(matches.pop())
}

/// Attribute the BOOTSEL volume that appeared after the 1200-bps touch.
/// Volumes recorded before the touch cannot belong to the board that was
/// just reset, so they are ignored; exactly one new volume is attributable,
/// several are not (one touch resets one board). With an empty `before` set
/// this is exactly the historical single-volume selection.
fn select_appeared_volume(
    before: &BTreeSet<PathBuf>,
    discovered: Vec<PathBuf>,
) -> Result<Option<PathBuf>> {
    let appeared: Vec<PathBuf> = discovered
        .into_iter()
        .filter(|volume| !before.contains(volume))
        .collect();
    select_single_uf2_volume(appeared)
}

/// Decide how the pre-touch BOOTSEL scan constrains the deploy. A single
/// mounted volume wins outright — the manual-BOOTSEL recovery flow
/// (FastLED/fbuild#1040) depends on that even when a runtime selector was
/// passed. Several volumes are only tolerated when a runtime port is about
/// to be touched, so the post-touch scan can attribute the volume that
/// newly appears; otherwise the historical hard error stands.
fn pretouch_volume_policy(
    mounted: Vec<PathBuf>,
    can_attribute: bool,
) -> Result<(Option<PathBuf>, BTreeSet<PathBuf>)> {
    if mounted.len() > 1 {
        if can_attribute {
            return Ok((None, mounted.into_iter().collect()));
        }
        return Err(multiple_bootsel_volumes_error(&mounted));
    }
    Ok((mounted.into_iter().next(), BTreeSet::new()))
}

fn explicit_uf2_volume(selector: &str) -> Option<PathBuf> {
    let candidate = selector
        .strip_prefix("UF2=")
        .or_else(|| selector.strip_prefix("uf2="))
        .unwrap_or(selector);
    let path = PathBuf::from(candidate);
    has_uf2_marker(&path).then_some(path)
}

fn touch_1200bps(port: &str) -> Result<()> {
    match serialport::new(port, 9600)
        .timeout(Duration::from_secs(2))
        .open()
    {
        Ok(mut serial) => {
            // Match Arduino-Pico's reset handshake: assert/deassert DTR,
            // switch to the magic baud, then close the handle.
            serial.write_data_terminal_ready(true).map_err(|error| {
                FbuildError::SerialError(format!(
                    "failed to assert DTR while resetting {port}: {error}"
                ))
            })?;
            std::thread::sleep(Duration::from_millis(100));
            serial.write_data_terminal_ready(false).map_err(|error| {
                FbuildError::SerialError(format!(
                    "failed to deassert DTR while resetting {port}: {error}"
                ))
            })?;
            serial.set_baud_rate(1200).map_err(|error| {
                FbuildError::SerialError(format!(
                    "failed to set the RP2040 reset baud on {port}: {error}"
                ))
            })?;
            Ok(())
        }
        Err(error) if error.kind() == serialport::ErrorKind::NoDevice => Ok(()),
        Err(error) => Err(FbuildError::SerialError(format!(
            "failed to enter RP2040 BOOTSEL on {port}: {error}"
        ))),
    }
}

/// macOS 13+ on Apple Silicon holds a first-seen USB accessory off the bus
/// until the user approves it, which reads as a silent BOOTSEL discovery
/// timeout (FastLED/fbuild#1082).
fn macos_accessory_hint(is_macos: bool) -> &'static str {
    if is_macos {
        ". On macOS 13+ (Apple Silicon), a first-seen USB accessory must be approved (\"Allow accessory to connect?\") before it enumerates; check for that prompt and System Settings > Privacy & Security > Accessories"
    } else {
        ""
    }
}

fn bootsel_not_found_message(is_macos: bool) -> String {
    format!(
        "RP2040 BOOTSEL volume not found; check that the stock board is connected and retry (discovery window is extendable with {BOOTLOADER_TIMEOUT_ENV}){}",
        macos_accessory_hint(is_macos)
    )
}

fn select_volume_after_reset(
    volume: Option<PathBuf>,
    reset_error: Option<FbuildError>,
) -> Result<PathBuf> {
    if let Some(volume) = volume {
        if let Some(error) = reset_error {
            // Windows can invalidate the CDC handle while processing the
            // final SET_LINE_CODING request because the device has already
            // acted on the 1200-bps touch and disconnected. The newly
            // discovered ROM volume is the authoritative success signal.
            tracing::warn!(
                reset_error = %error,
                volume = %volume.display(),
                "RP2040 1200-bps reset reported an error, but BOOTSEL appeared; continuing"
            );
        }
        return Ok(volume);
    }

    if let Some(error) = reset_error {
        return Err(FbuildError::DeployFailed(format!(
            "{error}; no RP2040 BOOTSEL transition was observed after the 1200-bps reset (discovery window is extendable with {BOOTLOADER_TIMEOUT_ENV}){}",
            macos_accessory_hint(cfg!(target_os = "macos"))
        )));
    }

    Err(FbuildError::DeployFailed(bootsel_not_found_message(cfg!(
        target_os = "macos"
    ))))
}

fn prepare_uf2_artifact(firmware_path: &Path, family_id: u32) -> Result<(PathBuf, Uf2Target)> {
    let input_is_uf2 = firmware_path
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("uf2"));
    let input_is_bin = firmware_path
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("bin"));
    if !input_is_uf2 && !input_is_bin {
        return Err(FbuildError::DeployFailed(format!(
            "unsupported RP2040 firmware input {}; expected a managed .uf2 or raw .bin artifact",
            firmware_path.display()
        )));
    }
    let input_bytes = fs::read(firmware_path).map_err(|error| {
        FbuildError::DeployFailed(format!(
            "failed to read {}: {error}",
            firmware_path.display()
        ))
    })?;
    let sibling_uf2 = firmware_path.with_extension("uf2");
    let (artifact, uf2) = if input_is_uf2 {
        reject_stale_uf2(firmware_path)?;
        (firmware_path.to_path_buf(), input_bytes)
    } else if sibling_uf2.is_file() {
        reject_stale_uf2(&sibling_uf2)?;
        let bytes = fs::read(&sibling_uf2).map_err(|error| {
            FbuildError::DeployFailed(format!(
                "failed to read managed RP2040 UF2 {}: {error}",
                sibling_uf2.display()
            ))
        })?;
        (sibling_uf2, bytes)
    } else {
        let bytes = encode_uf2_for_family(&input_bytes, family_id);
        let artifact = firmware_path.with_extension("uf2");
        fs::write(&artifact, &bytes).map_err(|error| {
            FbuildError::DeployFailed(format!(
                "failed to write RP2040 UF2 artifact {}: {error}",
                artifact.display()
            ))
        })?;
        (artifact, bytes)
    };
    let target = validate_uf2(&uf2, family_id)?;

    Ok((artifact, target))
}

fn copy_prepared_uf2(artifact: &Path, volume: &Path) -> Result<PathBuf> {
    let destination = volume.join("NEW.UF2");
    copy_uf2_artifact(artifact, &destination, volume)?;
    Ok(destination)
}

/// Successful mass-storage transfer: where NEW.UF2 landed and which mounted
/// volume accepted it (a retry may land on a re-enumerated volume path).
#[derive(Debug)]
struct MscTransfer {
    destination: PathBuf,
    volume: PathBuf,
}

/// Mass-storage transfer failure that survived the retry policy.
#[derive(Debug)]
struct MscTransferFailure {
    error: FbuildError,
    volume: PathBuf,
    attempts: usize,
}

/// Write NEW.UF2 with bounded retries. A retry is only taken across a fresh
/// ROM enumeration: the failed volume must first drop off the host
/// (`volume_gone`) and a BOOTSEL volume must then re-appear (`rediscover`).
/// Re-writing into a mount that never dropped would repeat the same wedged
/// transport (see the Windows error-121 guidance), so that case fails over
/// to the managed picotool fallback instead.
fn transfer_uf2_with_retries<C, G, R>(
    initial_volume: PathBuf,
    max_attempts: usize,
    mut copy: C,
    mut volume_gone: G,
    mut rediscover: R,
) -> std::result::Result<MscTransfer, MscTransferFailure>
where
    C: FnMut(&Path) -> Result<PathBuf>,
    G: FnMut(&Path) -> bool,
    R: FnMut() -> Result<Option<PathBuf>>,
{
    let mut volume = initial_volume;
    let mut attempts = 0;
    loop {
        attempts += 1;
        match copy(&volume) {
            Ok(destination) => {
                return Ok(MscTransfer {
                    destination,
                    volume,
                });
            }
            Err(error) => {
                if attempts >= max_attempts || !volume_gone(&volume) {
                    return Err(MscTransferFailure {
                        error,
                        volume,
                        attempts,
                    });
                }
                match rediscover() {
                    Ok(Some(next)) => {
                        tracing::warn!(
                            error = %error,
                            volume = %next.display(),
                            attempt = attempts,
                            "RP2040 BOOTSEL re-enumerated after a failed UF2 transfer; retrying"
                        );
                        volume = next;
                    }
                    _ => {
                        return Err(MscTransferFailure {
                            error,
                            volume,
                            attempts,
                        });
                    }
                }
            }
        }
    }
}

#[derive(Debug)]
struct WatchdogWorker {
    timed_out_at: Option<Instant>,
}

#[derive(Debug, Default)]
struct WatchdogWorkers {
    next_id: u64,
    active: HashMap<u64, WatchdogWorker>,
}

fn watchdog_workers() -> &'static Mutex<WatchdogWorkers> {
    static WORKERS: OnceLock<Mutex<WatchdogWorkers>> = OnceLock::new();
    WORKERS.get_or_init(|| Mutex::new(WatchdogWorkers::default()))
}

fn watchdog_workers_lock() -> std::sync::MutexGuard<'static, WatchdogWorkers> {
    watchdog_workers()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn register_watchdog_worker() -> u64 {
    let mut workers = watchdog_workers_lock();
    let id = workers.next_id;
    workers.next_id += 1;
    workers
        .active
        .insert(id, WatchdogWorker { timed_out_at: None });
    id
}

fn complete_watchdog_worker(id: u64) {
    watchdog_workers_lock().active.remove(&id);
}

fn mark_watchdog_worker_abandoned(id: u64) {
    if let Some(worker) = watchdog_workers_lock().active.get_mut(&id) {
        worker.timed_out_at.get_or_insert_with(Instant::now);
    }
}

/// A deploy-facing diagnostic for workers that outlived their watchdog grace
/// period. The count is live, rather than historical: it returns to zero when
/// a delayed kernel write finally unwinds and closes its destination handle.
fn watchdog_diagnostics() -> String {
    let workers = watchdog_workers_lock();
    let now = Instant::now();
    let mut abandoned = workers
        .active
        .values()
        .filter_map(|worker| {
            worker
                .timed_out_at
                .map(|at| now.saturating_duration_since(at))
        })
        .collect::<Vec<_>>();
    abandoned.sort_unstable();
    match abandoned.last() {
        Some(oldest) => format!(
            "RP2040 UF2 watchdog diagnostics: {} abandoned worker(s); oldest abandoned {}ms ago",
            abandoned.len(),
            oldest.as_millis()
        ),
        None => "RP2040 UF2 watchdog diagnostics: no abandoned workers".to_string(),
    }
}

#[cfg(windows)]
fn cancel_synchronous_io(worker: &std::thread::JoinHandle<()>) {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::System::IO::CancelSynchronousIo;

    // SAFETY: `as_raw_handle` is valid for the lifetime of `worker`; Windows
    // documents CancelSynchronousIo as safe to call on another thread handle.
    unsafe {
        let _ = CancelSynchronousIo(worker.as_raw_handle() as isize);
    }
}

#[cfg(not(windows))]
fn cancel_synchronous_io(_worker: &std::thread::JoinHandle<()>) {}

/// Run `work` on a dedicated thread and give up after `budget`. A storport
/// retry storm behind a sick hub can block the NEW.UF2 `write_all` for
/// minutes with no output. On Windows a timeout asks the kernel to cancel the
/// synchronous I/O and gives the worker a bounded grace period to close the
/// destination. If that is not possible, the daemon reports the live
/// abandoned-worker count and age rather than hiding a possible held handle.
fn run_with_watchdog<T: Send + 'static>(
    budget: Duration,
    label: &str,
    work: impl FnOnce() -> Result<T> + Send + 'static,
) -> Result<T> {
    let (sender, receiver) = std::sync::mpsc::channel();
    let worker_id = register_watchdog_worker();
    let worker = std::thread::spawn(move || {
        let result = work();
        complete_watchdog_worker(worker_id);
        let _ = sender.send(result);
    });
    match receiver.recv_timeout(budget) {
        Ok(result) => {
            let _ = worker.join();
            result
        }
        Err(error) => {
            if matches!(error, std::sync::mpsc::RecvTimeoutError::Timeout) {
                cancel_synchronous_io(&worker);
                match receiver.recv_timeout(WATCHDOG_CANCELLATION_GRACE) {
                    Ok(_) | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                        let _ = worker.join();
                    }
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                        mark_watchdog_worker_abandoned(worker_id);
                    }
                }
            } else {
                let _ = worker.join();
            }
            let timeout_message = format!(
                "{label} did not complete within {}s; the storage transport is likely wedged — request a fresh USB enumeration before retrying (override the budget with {UF2_WRITE_TIMEOUT_ENV})",
                budget.as_secs()
            );
            Err(FbuildError::DeployFailed(format!(
                "{timeout_message}; {}",
                watchdog_diagnostics()
            )))
        }
    }
}

fn describe_transfer_location(volume: Option<&Path>) -> String {
    match volume {
        Some(volume) => volume.display().to_string(),
        None => "PICOBOOT vendor interface".to_string(),
    }
}

/// Append a captured USB topology line (or an explicit "unavailable" marker
/// when capture failed) to a failure context, so hub-path failures state
/// what fbuild does and doesn't know about the physical connection
/// (FastLED/fbuild#1081, #1082).
fn with_topology(context: String, topology: Option<&str>) -> String {
    match topology {
        Some(topology) => format!("{context}. {topology}"),
        None => format!("{context}. USB topology unavailable"),
    }
}

#[cfg(test)]
fn write_uf2(firmware_path: &Path, volume: &Path, family_id: u32) -> Result<PathBuf> {
    let (artifact, _) = prepare_uf2_artifact(firmware_path, family_id)?;
    copy_prepared_uf2(&artifact, volume)
}

fn reject_stale_uf2(uf2: &Path) -> Result<()> {
    let uf2_modified = fs::metadata(uf2)
        .and_then(|metadata| metadata.modified())
        .map_err(|error| {
            FbuildError::DeployFailed(format!(
                "failed to inspect RP2040 UF2 artifact {}: {error}",
                uf2.display()
            ))
        })?;
    for extension in ["elf", "bin"] {
        let source = uf2.with_extension(extension);
        let Ok(source_modified) = fs::metadata(&source).and_then(|metadata| metadata.modified())
        else {
            continue;
        };
        if source_modified > uf2_modified {
            return Err(FbuildError::DeployFailed(format!(
                "RP2040 UF2 {} is older than {}; rebuild without --skip-build before deploying",
                uf2.display(),
                source.display()
            )));
        }
    }
    Ok(())
}

fn copy_uf2_artifact(artifact: &Path, destination: &Path, volume: &Path) -> Result<()> {
    copy_uf2_artifact_with(artifact, destination, volume, |source, target| {
        write_uf2_artifact_direct(source, target)
    })
}

#[derive(Debug)]
struct Uf2WriteFailure {
    error: io::Error,
    bytes_written: u64,
}

impl Uf2WriteFailure {
    fn new(error: io::Error, bytes_written: u64) -> Self {
        Self {
            error,
            bytes_written,
        }
    }
}

fn write_uf2_artifact_direct(
    artifact: &Path,
    destination: &Path,
) -> std::result::Result<u64, Uf2WriteFailure> {
    // Match Arduino-Pico's uf2conv.py: hold the completed UF2 in memory,
    // create/truncate NEW.UF2, write it sequentially, flush, and close. Avoid
    // filesystem-level copy, metadata preservation, fsync, rename, or readback
    // on the ROM-emulated FAT volume.
    let bytes = fs::read(artifact).map_err(|error| Uf2WriteFailure::new(error, 0))?;
    let output =
        open_uf2_destination(destination).map_err(|error| Uf2WriteFailure::new(error, 0))?;
    write_uf2_bytes(output, &bytes)
}

fn open_uf2_destination(destination: &Path) -> io::Result<fs::File> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);

    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;

        // CPython's open(name, "wb") reaches UCRT _wopen(), whose default
        // _SH_DENYNO mapping is FILE_SHARE_READ | FILE_SHARE_WRITE. Rust also
        // enables FILE_SHARE_DELETE by default. Exclude that extra permission
        // so removable-drive scanners cannot delete/replace NEW.UF2 while the
        // ROM transfer handle is active and the Windows path matches the
        // Arduino-Pico uploader exactly.
        const FILE_SHARE_READ: u32 = 0x0000_0001;
        const FILE_SHARE_WRITE: u32 = 0x0000_0002;
        options.share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE);
    }

    options.open(destination)
}

struct CountingWriter<W> {
    inner: W,
    bytes_written: u64,
}

impl<W: Write> Write for CountingWriter<W> {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        let written = self.inner.write(buffer)?;
        self.bytes_written += written as u64;
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

fn write_uf2_bytes<W: Write>(output: W, bytes: &[u8]) -> std::result::Result<u64, Uf2WriteFailure> {
    // A large CPython BufferedWriter write bypasses its small buffer and sends
    // this whole byte slice to the raw file in one call. write_all has the
    // same first-call shape and only retries if the OS reports a short write.
    let mut output = CountingWriter {
        inner: output,
        bytes_written: 0,
    };
    if let Err(error) = output.write_all(bytes) {
        return Err(Uf2WriteFailure::new(error, output.bytes_written));
    }
    output
        .flush()
        .map_err(|error| Uf2WriteFailure::new(error, output.bytes_written))?;
    Ok(output.bytes_written)
}

fn copy_uf2_artifact_with<F>(
    artifact: &Path,
    destination: &Path,
    volume: &Path,
    copy: F,
) -> Result<()>
where
    F: FnOnce(&Path, &Path) -> std::result::Result<u64, Uf2WriteFailure>,
{
    let expected_bytes = fs::metadata(artifact)
        .map_err(|error| {
            FbuildError::DeployFailed(format!(
                "failed to inspect RP2040 UF2 {} before transfer: {error}",
                artifact.display()
            ))
        })?
        .len();
    match copy(artifact, destination) {
        Ok(bytes_written) if bytes_written == expected_bytes => Ok(()),
        Ok(bytes_written) => Err(FbuildError::DeployFailed(format!(
            "failed to copy RP2040 UF2 {} to {}: writer reported {bytes_written} of {expected_bytes} bytes without an error",
            artifact.display(),
            destination.display()
        ))),
        Err(write_failure) => {
            // Some hosts report a final I/O error when the ROM ejects the
            // virtual FAT volume immediately after accepting the last block.
            // Only accept that race after the host reported every source byte
            // written and the marker disappeared. Otherwise preserve the
            // original actionable write error.
            if write_failure.bytes_written == expected_bytes
                && is_device_disappearance_error(&write_failure.error)
                && wait_for_volume_disappearance(volume, Duration::from_secs(2)).is_ok()
            {
                tracing::debug!(
                    error = %write_failure.error,
                    volume = %volume.display(),
                    "RP2040 volume ejected while the host finalized NEW.UF2; treating transfer as accepted"
                );
                Ok(())
            } else {
                Err(FbuildError::DeployFailed(format_uf2_copy_error(
                    artifact,
                    destination,
                    &write_failure.error,
                )))
            }
        }
    }
}

fn format_uf2_copy_error(
    artifact: &Path,
    destination: &Path,
    copy_error: &std::io::Error,
) -> String {
    let base = format!(
        "failed to copy RP2040 UF2 {} to {}: {copy_error}",
        artifact.display(),
        destination.display()
    );
    match copy_error.raw_os_error() {
        Some(5) => format!(
            "{base}. Windows denied write access to the RP-series BOOTSEL volume (error 5). This is characteristically a host removable-storage write-deny policy — Group Policy \"Removable Disks: Deny write access\" or BitLocker FDVDenyWriteAccess — or an aggressive endpoint-protection filter, not a board fault. Check with this machine's administrator; retrying on a direct USB port will not clear a policy block"
        ),
        Some(121) => format!(
            "{base}. Windows timed out writing to the RP-series BOOTSEL storage transport (error 121), and fbuild did not observe the ROM eject transition. Request a fresh USB enumeration on a direct USB port with a known data cable, avoid USB hubs for the retry, and do not retry the same timed-out enumeration. A blank or invalid-flash Pico returns to ROM boot automatically: reconnect normally and do not press BOOTSEL"
        ),
        Some(1006) => format!(
            "{base}. Windows invalidated the open handle to the RP-series BOOTSEL synthetic FAT volume (error 1006), and fbuild did not observe the ROM eject transition. Request a fresh USB enumeration on a direct USB port and close software that scans or synchronizes removable drives before retrying. A blank or invalid-flash Pico returns to ROM boot automatically: reconnect normally and do not press BOOTSEL"
        ),
        Some(1392) => format!(
            "{base}. Windows cannot access the RP-series BOOTSEL synthetic FAT volume (error 1392). Do not run chkdsk, filesystem repair, or format this ROM-emulated volume; request a fresh USB enumeration and retry, or use fbuild's managed picotool fallback with the Raspberry Pi-documented WinUSB binding. A blank or invalid-flash Pico returns to ROM boot automatically: reconnect normally and do not press BOOTSEL"
        ),
        _ => base,
    }
}

fn is_device_disappearance_error(error: &std::io::Error) -> bool {
    use std::io::ErrorKind;

    matches!(
        error.kind(),
        ErrorKind::NotFound
            | ErrorKind::BrokenPipe
            | ErrorKind::ConnectionAborted
            | ErrorKind::ConnectionReset
            | ErrorKind::UnexpectedEof
    ) || matches!(
        error.raw_os_error(),
        // Windows: FILE/PATH_NOT_FOUND, INVALID_HANDLE, NOT_READY,
        // FILE_INVALID after eject, DEVICE_NOT_CONNECTED. Unix: ENODEV.
        Some(2 | 3 | 6 | 21 | 1006 | 1167 | 19)
    )
}

fn validate_uf2(bytes: &[u8], expected_family: u32) -> Result<Uf2Target> {
    if bytes.is_empty() || bytes.len() % UF2_BLOCK_SIZE != 0 {
        return Err(FbuildError::DeployFailed(format!(
            "malformed RP2040 UF2: size {} is not a non-zero multiple of {UF2_BLOCK_SIZE}",
            bytes.len()
        )));
    }
    let block_count = bytes.len() / UF2_BLOCK_SIZE;
    let mut seen = vec![false; block_count];
    let mut seen_ranges = Vec::with_capacity(block_count);
    let mut image_target = None;
    let flash_end = if expected_family == RP2350_FAMILY_ID {
        0x1400_0000u32
    } else {
        0x1100_0000u32
    };
    for (index, block) in bytes.chunks_exact(UF2_BLOCK_SIZE).enumerate() {
        let field = |offset: usize| {
            u32::from_le_bytes(
                block[offset..offset + 4]
                    .try_into()
                    .expect("four-byte UF2 field"),
            )
        };
        if field(0) != UF2_MAGIC_START0
            || field(4) != UF2_MAGIC_START1
            || field(508) != UF2_MAGIC_END
        {
            return Err(FbuildError::DeployFailed(format!(
                "malformed RP2040 UF2: invalid magic in block {index}"
            )));
        }
        if field(8) & UF2_FLAG_FAMILY_ID_PRESENT == 0 || field(28) != expected_family {
            return Err(FbuildError::DeployFailed(format!(
                "wrong UF2 family in block {index}: expected 0x{expected_family:08X}, found 0x{:08X}",
                field(28)
            )));
        }
        let target_address = field(12);
        let Some(target_end) = target_address.checked_add(UF2_PAYLOAD_SIZE as u32) else {
            return Err(FbuildError::DeployFailed(format!(
                "malformed RP2040 UF2 block metadata at block {index}"
            )));
        };
        let block_number = field(20) as usize;
        let is_flash = target_address >= RP2040_UF2_BASE_ADDRESS
            && target_end <= flash_end
            && target_address % UF2_PAYLOAD_SIZE as u32 == 0;
        let is_rp2040_ram = expected_family == RP2040_FAMILY_ID
            && ((target_address >= RP2040_XIP_SRAM_START && target_end <= RP2040_XIP_SRAM_END)
                || (target_address >= RP2040_SRAM_START && target_end <= RP2040_SRAM_END));
        let block_target = if is_flash {
            Uf2Target::Flash
        } else if is_rp2040_ram {
            Uf2Target::Ram
        } else {
            return Err(FbuildError::DeployFailed(format!(
                "malformed RP2040 UF2 block metadata at block {index}"
            )));
        };
        if field(16) != UF2_PAYLOAD_SIZE as u32
            || block_number >= block_count
            || field(24) as usize != block_count
        {
            return Err(FbuildError::DeployFailed(format!(
                "malformed RP2040 UF2 block metadata at block {index}"
            )));
        }
        if image_target.is_some_and(|target| target != block_target) {
            return Err(FbuildError::DeployFailed(
                "malformed RP2040 UF2: image mixes flash and RAM target addresses".to_string(),
            ));
        }
        image_target = Some(block_target);
        if std::mem::replace(&mut seen[block_number], true) {
            return Err(FbuildError::DeployFailed(format!(
                "malformed RP2040 UF2: duplicate block number {block_number}"
            )));
        }
        seen_ranges.push((target_address, target_end));
    }
    if seen.iter().any(|present| !present) {
        return Err(FbuildError::DeployFailed(
            "malformed RP2040 UF2: block-number sequence is incomplete".to_string(),
        ));
    }
    seen_ranges.sort_unstable_by_key(|&(start, _)| start);
    if let Some(overlap) = seen_ranges
        .windows(2)
        .find(|ranges| ranges[1].0 < ranges[0].1)
    {
        return Err(FbuildError::DeployFailed(format!(
            "malformed RP2040 UF2: overlapping target address 0x{:08X}",
            overlap[1].0
        )));
    }
    Ok(image_target.expect("non-empty UF2 has at least one block"))
}

fn ram_load_result(
    volume: Option<&Path>,
    transfer_method: &str,
    stdout: String,
    stderr: String,
) -> DeploymentResult {
    DeploymentResult {
        success: true,
        message: format!(
            "RP2040 RAM image accepted for execution via {transfer_method} ({})",
            describe_transfer_location(volume),
        ),
        port: None,
        stdout,
        stderr,
        outcome: DeployOutcome::RamLoad,
    }
}

fn wait_for_volume_disappearance(volume: &Path, timeout: Duration) -> Result<()> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if !has_uf2_marker(volume) {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Err(FbuildError::DeployFailed(format!(
        "RP2040 BOOTSEL volume {} did not eject after NEW.UF2; deployment is unconfirmed. This symptom alone does not identify a QSPI flash fault or any other root cause; capture the transfer error and reproduce with an independently verified target before diagnosis",
        volume.display()
    )))
}

/// Deploys RP2040-family firmware through the stock BOOTSEL mass-storage
/// interface. `bootloader_timeout` is configurable for deterministic tests.
pub struct Rp2040Deployer {
    bootloader_timeout: Duration,
    post_deploy_timeout: Duration,
    uf2_write_timeout: Duration,
    family_id: u32,
}

impl Default for Rp2040Deployer {
    fn default() -> Self {
        Self {
            bootloader_timeout: timeout_from_env(BOOTLOADER_TIMEOUT_ENV, Duration::from_secs(10)),
            post_deploy_timeout: rp2040_post_deploy_timeout(),
            uf2_write_timeout: timeout_from_env(UF2_WRITE_TIMEOUT_ENV, DEFAULT_UF2_WRITE_TIMEOUT),
            family_id: RP2040_FAMILY_ID,
        }
    }
}

impl Rp2040Deployer {
    pub fn new(bootloader_timeout: Duration, post_deploy_timeout: Duration) -> Self {
        Self {
            bootloader_timeout,
            post_deploy_timeout,
            ..Self::default()
        }
    }

    pub fn for_mcu(mcu: &str) -> Result<Self> {
        let mut deployer = Self::default();
        match mcu.to_ascii_lowercase().as_str() {
            value if value.starts_with("rp2040") => {}
            value if value.starts_with("rp2350") => deployer.family_id = RP2350_FAMILY_ID,
            _ => {
                return Err(FbuildError::DeployFailed(format!(
                    "unsupported Raspberry Pi MCU {mcu:?}; expected rp2040 or rp2350"
                )));
            }
        }
        Ok(deployer)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PicoCdcPort {
    name: String,
    serial_number: Option<String>,
    health: fbuild_serial::ports::PortHealth,
    instance_id: Option<String>,
    parent_instance_id: Option<String>,
}

fn catalogue_pico_cdc_ports(expected_family: u32) -> Result<Vec<PicoCdcPort>> {
    let ports = fbuild_serial::ports::available_ports().map_err(|error| {
        FbuildError::SerialError(format!(
            "failed to enumerate post-deploy serial ports: {error}"
        ))
    })?;
    let mut matches: Vec<PicoCdcPort> = ports
        .into_iter()
        .filter_map(|port| {
            let serialport::SerialPortType::UsbPort(usb) = &port.info.port_type else {
                return None;
            };
            let matches_family = fbuild_core::usb::profiles::profiles_for(usb.vid, usb.pid)
                .iter()
                .any(|profile| profile_matches_family(profile, expected_family));
            matches_family.then(|| PicoCdcPort {
                name: port.info.port_name,
                serial_number: usb.serial_number.clone(),
                health: port.health,
                instance_id: port.instance_id,
                parent_instance_id: port.parent_instance_id,
            })
        })
        .collect();
    matches.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(matches)
}

/// CDC-wait failure classification: a quiet window (`Timeout`) is
/// recoverable once the flash itself is confirmed, while an enumeration or
/// selection error (`Enumeration`) always fails the deploy.
#[derive(Debug)]
enum CdcWaitError {
    Timeout(CdcWaitTimeout),
    Enumeration(FbuildError),
}

#[derive(Debug)]
struct CdcWaitTimeout {
    elapsed: Duration,
    previous_port: Option<String>,
    requested_serial: Option<String>,
    candidates: Vec<PicoCdcPort>,
    /// The most recent bounded-open failure on an otherwise eligible
    /// candidate, as `"<port>: <error>"` (FastLED/fbuild#1147: healthy
    /// metadata alone never proves a live endpoint).
    last_open_error: Option<String>,
}

impl CdcWaitTimeout {
    fn diagnostics(&self) -> String {
        let prior_port = self.previous_port.as_deref().unwrap_or("none");
        let requested_serial = self.requested_serial.as_deref().unwrap_or("none");
        let candidates = if self.candidates.is_empty() {
            "none".to_string()
        } else {
            self.candidates
                .iter()
                .map(|candidate| {
                    let serial = candidate
                        .serial_number
                        .as_deref()
                        .map(|value| format!("serial {value}"))
                        .unwrap_or_else(|| "serial unavailable".to_string());
                    let instance = candidate
                        .instance_id
                        .as_deref()
                        .map(|value| format!("; instance {value}"))
                        .unwrap_or_default();
                    let parent = candidate
                        .parent_instance_id
                        .as_deref()
                        .map(|value| format!("; parent {value}"))
                        .unwrap_or_default();
                    format!(
                        "{} ({serial}; health {}{instance}{parent})",
                        candidate.name,
                        candidate.health.label()
                    )
                })
                .collect::<Vec<_>>()
                .join(", ")
        };
        let open_error = self
            .last_open_error
            .as_deref()
            .map(|value| format!("; last open error {value}"))
            .unwrap_or_default();
        format!(
            "elapsed {}ms; prior port {prior_port}; requested serial {requested_serial}; catalogue candidates: {candidates}{open_error}",
            self.elapsed.as_millis()
        )
    }

    /// Manual recovery guidance for a confirmed flash whose runtime CDC never
    /// became a healthy, openable endpoint (FastLED/fbuild#1147 step 5). When
    /// Windows retained stale devnode records for this board, name them so the
    /// user sees exactly why the historical COM port was not returned.
    fn recovery_guidance(&self) -> String {
        let stale: Vec<String> = self
            .candidates
            .iter()
            .filter(|candidate| candidate.health.is_known_unhealthy())
            .map(describe_unhealthy)
            .collect();
        let stale_note = if stale.is_empty() {
            String::new()
        } else {
            format!(
                " Windows retained stale/problem devnode record(s) for this board: {}; a historical COM name with a matching serial is not a live endpoint and was not returned.",
                stale.join(", ")
            )
        };
        format!(
            "{stale_note} To recover manually: use a direct motherboard USB port and a known-good data cable, then 1) hold BOOT/BOOTSEL, 2) press and release RESET while still holding BOOT, 3) keep BOOT held for about two seconds, then release, 4) verify an RPI-RP2 volume appears and rerun the deploy."
        )
    }
}

fn wait_for_cdc_port(
    previous_port: Option<&str>,
    requested_serial: Option<&str>,
    before: &BTreeSet<String>,
    expected_family: u32,
    timeout: Duration,
) -> std::result::Result<PicoCdcPort, CdcWaitError> {
    let started = Instant::now();
    wait_for_cdc_port_with_clock(
        previous_port,
        requested_serial,
        before,
        timeout,
        || catalogue_pico_cdc_ports(expected_family),
        probe_cdc_openable,
        || started.elapsed(),
        std::thread::sleep,
    )
}

/// Bounded openability probe (FastLED/fbuild#1147): health metadata alone
/// never proves a live endpoint, so the selected candidate must open before
/// it may become `DeploymentResult::port`.
fn probe_cdc_openable(port: &PicoCdcPort) -> std::result::Result<(), String> {
    serialport::new(&port.name, 115_200)
        .timeout(Duration::from_millis(250))
        .open()
        .map(drop)
        .map_err(|error| error.to_string())
}

#[allow(clippy::too_many_arguments)]
fn wait_for_cdc_port_with_clock<F, P, E, S>(
    previous_port: Option<&str>,
    requested_serial: Option<&str>,
    before: &BTreeSet<String>,
    timeout: Duration,
    mut catalogue: F,
    mut probe: P,
    mut elapsed: E,
    mut sleep: S,
) -> std::result::Result<PicoCdcPort, CdcWaitError>
where
    F: FnMut() -> Result<Vec<PicoCdcPort>>,
    P: FnMut(&PicoCdcPort) -> std::result::Result<(), String>,
    E: FnMut() -> Duration,
    S: FnMut(Duration),
{
    let mut last_open_error: Option<String> = None;
    loop {
        let ports = catalogue().map_err(CdcWaitError::Enumeration)?;
        match select_cdc_candidate(previous_port, requested_serial, before, &ports) {
            // A candidate that fails its bounded open probe is not returned;
            // the loop keeps polling so a transient just-enumerated failure
            // can clear, and the last failure lands in the timeout
            // diagnostics (FastLED/fbuild#1147).
            Ok(Some(selected)) => match probe(&selected) {
                Ok(()) => return Ok(selected),
                Err(error) => {
                    last_open_error = Some(format!("{}: {error}", selected.name));
                }
            },
            Ok(None) => {}
            Err(error) => return Err(CdcWaitError::Enumeration(error)),
        }
        let waited = elapsed();
        if waited >= timeout {
            return Err(CdcWaitError::Timeout(CdcWaitTimeout {
                elapsed: waited,
                previous_port: previous_port.map(str::to_string),
                requested_serial: requested_serial.map(str::to_string),
                candidates: ports,
                last_open_error,
            }));
        }
        sleep(CDC_POLL_INTERVAL.min(timeout.saturating_sub(waited)));
    }
}

/// Post-flash CDC state: either the runtime port was re-identified, or the
/// flash is confirmed good but the port never showed inside the window.
#[derive(Debug)]
enum PostFlashCdc {
    Confirmed(String),
    /// Carries the message fragment explaining the downgrade.
    Unconfirmed(String),
}

/// Decide the deploy outcome after the runtime-CDC wait (FastLED/fbuild#1082
/// stage 7). Once the eject watch (or a PICOBOOT load) confirmed the ROM
/// accepted the image, a quiet CDC window must not fail the deploy: first-plug
/// driver installation routinely exceeds it, and a hard failure makes CI
/// re-flash a healthy board. Enumeration errors still fail regardless.
fn resolve_post_flash_cdc(
    flash_confirmed: bool,
    wait_result: std::result::Result<PicoCdcPort, CdcWaitError>,
    window: Duration,
) -> Result<PostFlashCdc> {
    match wait_result {
        Ok(port) => Ok(PostFlashCdc::Confirmed(port.name)),
        Err(CdcWaitError::Enumeration(error)) => Err(error),
        Err(CdcWaitError::Timeout(diagnostics)) if flash_confirmed => {
            tracing::warn!(diagnostics = %diagnostics.diagnostics(), "RP2040 runtime CDC did not return before the confirmed-flash deadline");
            Ok(PostFlashCdc::Unconfirmed(format!(
                "the firmware was flashed and accepted, but no healthy, openable runtime CDC port reappeared within {}s ({}).{} First-plug driver installation can also exceed this window (extend it with {POST_DEPLOY_TIMEOUT_ENV})",
                window.as_secs(),
                diagnostics.diagnostics(),
                diagnostics.recovery_guidance(),
            )))
        }
        Err(CdcWaitError::Timeout(diagnostics)) => {
            tracing::warn!(diagnostics = %diagnostics.diagnostics(), "RP2040 runtime CDC did not return before the unconfirmed-flash deadline");
            Err(FbuildError::DeployFailed(format!(
                "RP2040 firmware was transferred, but no catalogue-identified runtime CDC port appeared within {}s; verify that the firmware enables USB serial and that FastLED/boards USB data is current (extend the window with {POST_DEPLOY_TIMEOUT_ENV})",
                window.as_secs()
            )))
        }
    }
}

fn profile_matches_family(
    profile: &fbuild_core::usb::profiles::UsbTransportProfile,
    family_id: u32,
) -> bool {
    use fbuild_core::usb::profiles::{UsbDeviceRole, UsbPurpose};

    if profile.purpose != UsbPurpose::Runtime
        || profile.role != UsbDeviceRole::RuntimeCdc
        || profile.interface.as_deref() != Some("cdc")
    {
        return false;
    }
    let expected = if family_id == RP2350_FAMILY_ID {
        "rp2350"
    } else {
        "rp2040"
    };
    profile.family.as_deref() == Some(expected)
}

fn ensure_verified_usb_profiles() -> Result<()> {
    let dir = fbuild_paths::get_cache_root().join("usb");
    let meta_path = dir.join("_meta.json");
    let profiles_path = dir.join("usb-profiles.json");
    if fbuild_core::usb::profiles::populate_profiles_from_paths(&meta_path, &profiles_path) {
        Ok(())
    } else {
        Err(FbuildError::DeployFailed(
            "verified FastLED/boards USB profiles are unavailable; fbuild refuses to guess a Pico VID/PID or use a built-in fallback"
                .to_string(),
        ))
    }
}

#[async_trait::async_trait]
impl Deployer for Rp2040Deployer {
    async fn deploy(
        &self,
        project_dir: &Path,
        _env_name: &str,
        firmware_path: &Path,
        port: Option<&str>,
    ) -> Result<DeploymentResult> {
        tokio::task::spawn_blocking(ensure_verified_usb_profiles)
            .await
            .map_err(|error| {
                FbuildError::DeployFailed(format!(
                    "failed to prepare the FastLED/boards USB profile cache: {error}"
                ))
            })??;
        let selector = port.map(str::trim).filter(|value| !value.is_empty());
        let family_id = self.family_id;
        let current_ports =
            tokio::task::spawn_blocking(move || catalogue_pico_cdc_ports(family_id))
                .await
                .map_err(|error| {
                    FbuildError::DeployFailed(format!(
                        "RP2040 serial snapshot task failed: {error}"
                    ))
                })??;
        let ports_before: BTreeSet<String> =
            current_ports.iter().map(|port| port.name.clone()).collect();
        let explicit_volume = selector.and_then(explicit_uf2_volume);
        if selector.is_some_and(|value| value.to_ascii_lowercase().starts_with("uf2="))
            && explicit_volume.is_none()
        {
            return Err(FbuildError::DeployFailed(format!(
                "explicit RP2040 UF2 volume {selector:?} does not contain INFO_UF2.TXT"
            )));
        }
        let (volume_before_reset, volumes_before) = match explicit_volume {
            Some(volume) => (Some(volume), BTreeSet::new()),
            None => {
                let mounted = find_uf2_volumes(&volume_roots());
                // Attribution needs a live port to touch. A resolution
                // failure here falls back to the historical multi-volume
                // hard error rather than surfacing a selector error that a
                // single-volume deploy would never have raised.
                let can_attribute = mounted.len() > 1
                    && selector
                        .map(|value| resolve_requested_runtime_target(value, &current_ports))
                        .transpose()
                        .ok()
                        .flatten()
                        .is_some();
                pretouch_volume_policy(mounted, can_attribute)?
            }
        };
        let requested_serial = selector.and_then(serial_selector).map(str::to_string);
        let runtime_target = if volume_before_reset.is_none() {
            selector
                .map(|value| resolve_requested_runtime_target(value, &current_ports))
                .transpose()?
        } else {
            None
        };
        // Capture topology before the 1200-bps touch: once the board resets
        // into BOOTSEL the runtime CDC devnode this looks up disappears.
        // A join failure on this purely-diagnostic task must never fail the
        // deploy, so it degrades to `None` rather than propagating.
        let topology_port = runtime_target.as_ref().map(|target| target.port.clone());
        let topology: Option<String> = tokio::task::spawn_blocking(move || {
            topology_port.and_then(|port| topology::describe_port_topology(&port))
        })
        .await
        .unwrap_or(None);
        let reset_error = if let Some(target) = &runtime_target {
            let port = target.port.clone();
            tokio::task::spawn_blocking(move || touch_1200bps(&port))
                .await
                .map_err(|error| {
                    FbuildError::DeployFailed(format!("RP2040 reset task failed: {error}"))
                })?
                .err()
        } else {
            None
        };
        let (volume, volume_discovery_error) = if let Some(volume) = volume_before_reset {
            (Some(volume), None)
        } else {
            let timeout = self.bootloader_timeout;
            let stale_volumes = volumes_before.clone();
            let discovered =
                tokio::task::spawn_blocking(move || find_uf2_volume_until(timeout, &stale_volumes))
                    .await
                    .map_err(|error| {
                        FbuildError::DeployFailed(format!("RP2040 volume watcher failed: {error}"))
                    })??;
            match select_volume_after_reset(discovered, reset_error) {
                Ok(volume) => (Some(volume), None),
                // The synthetic FAT never mounted (common behind USB hubs,
                // FastLED/fbuild#1081). The PICOBOOT vendor interface may
                // still be reachable, so defer the failure until the managed
                // picotool fallback has had a chance.
                Err(error) => (None, Some(error)),
            }
        };
        let firmware = firmware_path.to_path_buf();
        let family_id = self.family_id;
        let (artifact, uf2_target) =
            tokio::task::spawn_blocking(move || prepare_uf2_artifact(&firmware, family_id))
                .await
                .map_err(|error| {
                    FbuildError::DeployFailed(format!(
                        "RP2040 UF2 preparation task failed: {error}"
                    ))
                })??;
        let (transfer_stdout, transfer_stderr, transfer_method, transfer_volume) =
            if let Some(volume) = volume {
                let artifact_for_copy = artifact.clone();
                let bootloader_timeout = self.bootloader_timeout;
                let write_budget = self.uf2_write_timeout;
                let stale_volumes = volumes_before.clone();
                let transfer = tokio::task::spawn_blocking(move || {
                    transfer_uf2_with_retries(
                        volume,
                        UF2_TRANSFER_ATTEMPTS,
                        // Watchdog per attempt: a timed-out (abandoned) write
                        // flows into the same fresh-enumeration retry gate as
                        // any other copy failure.
                        |volume| {
                            let artifact = artifact_for_copy.clone();
                            let volume = volume.to_path_buf();
                            run_with_watchdog(write_budget, "RP2040 UF2 write", move || {
                                copy_prepared_uf2(&artifact, &volume)
                            })
                        },
                        |volume| {
                            wait_for_volume_disappearance(volume, Duration::from_secs(2)).is_ok()
                        },
                        || find_uf2_volume_until(bootloader_timeout, &stale_volumes),
                    )
                })
                .await
                .map_err(|error| {
                    FbuildError::DeployFailed(format!("RP2040 UF2 writer task failed: {error}"))
                })?;
                match transfer {
                    Ok(transfer) => (
                        format!("wrote {}", transfer.destination.display()),
                        String::new(),
                        "BOOTSEL mass-storage",
                        Some(transfer.volume),
                    ),
                    Err(failure) => {
                        let context = with_topology(
                            format!(
                                "{} (after {} BOOTSEL transfer attempt(s))",
                                failure.error, failure.attempts
                            ),
                            topology.as_deref(),
                        );
                        let loaded =
                            picotool::load_with_managed_picotool(project_dir, &artifact, &context)
                                .await?;
                        (
                            loaded.stdout,
                            loaded.stderr,
                            "managed picotool fallback",
                            Some(failure.volume),
                        )
                    }
                }
            } else {
                let context = with_topology(
                    volume_discovery_error
                        .expect("missing volume implies a discovery error")
                        .to_string(),
                    topology.as_deref(),
                );
                let loaded =
                    picotool::load_with_managed_picotool(project_dir, &artifact, &context).await?;
                (
                    loaded.stdout,
                    loaded.stderr,
                    "managed picotool (BOOTSEL volume never mounted)",
                    None,
                )
            };
        if let Some(volume_for_wait) = transfer_volume.clone() {
            let post_timeout = self.post_deploy_timeout;
            tokio::task::spawn_blocking(move || {
                wait_for_volume_disappearance(&volume_for_wait, post_timeout)
            })
            .await
            .map_err(|error| {
                FbuildError::DeployFailed(format!("RP2040 eject watcher failed: {error}"))
            })??;
        }
        if uf2_target == Uf2Target::Ram {
            return Ok(ram_load_result(
                transfer_volume.as_deref(),
                transfer_method,
                transfer_stdout,
                transfer_stderr,
            ));
        }
        let recovery_port = runtime_target.as_ref().map(|target| target.port.clone());
        let recovery_serial = runtime_target
            .as_ref()
            .and_then(|target| target.serial_number.clone())
            .or(requested_serial);
        let family_id = self.family_id;
        let post_timeout = self.post_deploy_timeout;
        let wait_result = tokio::task::spawn_blocking(move || {
            wait_for_cdc_port(
                recovery_port.as_deref(),
                recovery_serial.as_deref(),
                &ports_before,
                family_id,
                post_timeout,
            )
        })
        .await
        .map_err(|error| {
            FbuildError::DeployFailed(format!("RP2040 CDC watcher failed: {error}"))
        })?;
        // Every mounted-volume path already passed the eject watch above, and
        // the volume-less path loaded through PICOBOOT; either signal confirms
        // the ROM accepted the image before the CDC wait started.
        let flash_confirmed =
            transfer_volume.is_some() || transfer_method.starts_with("managed picotool");
        let location = describe_transfer_location(transfer_volume.as_deref());
        let (message, port) =
            match resolve_post_flash_cdc(flash_confirmed, wait_result, post_timeout)? {
                PostFlashCdc::Confirmed(port) => (
                    format!("firmware deployed to RP2040 via {transfer_method} ({location})"),
                    Some(port),
                ),
                PostFlashCdc::Unconfirmed(note) => (
                    format!(
                        "firmware deployed to RP2040 via {transfer_method} ({location}); {note}"
                    ),
                    None,
                ),
            };
        Ok(DeploymentResult {
            success: true,
            message,
            port,
            stdout: transfer_stdout,
            stderr: transfer_stderr,
            outcome: DeployOutcome::FullFlash,
        })
    }

    /// The RP2040 deploy path rediscovers its post-flash endpoint through a
    /// fresh, health-gated, open-probed catalogue scan (FastLED/fbuild#1147);
    /// a `None` port after a successful flash means the runtime CDC was not
    /// recovered, and the pre-flash name must never be substituted.
    fn owns_post_flash_port_discovery(&self) -> bool {
        true
    }

    async fn post_deploy_recovery(&self, port: &str) -> Result<()> {
        let deadline = Instant::now() + self.post_deploy_timeout;
        while Instant::now() < deadline {
            let port_name = port.to_string();
            let present = tokio::task::spawn_blocking(move || {
                serialport::new(&port_name, 115_200)
                    .timeout(Duration::from_millis(100))
                    .open()
                    .is_ok()
            })
            .await
            .unwrap_or(false);
            if present {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        tracing::warn!("RP2040 CDC port {port} did not reappear after deploy");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    struct ShortWriter {
        max_write: usize,
    }

    impl Write for ShortWriter {
        fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
            Ok(buffer.len().min(self.max_write))
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    struct FlushFailureWriter;

    impl Write for FlushFailureWriter {
        fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
            Ok(buffer.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Err(io::Error::new(
                io::ErrorKind::NotFound,
                "volume ejected during flush",
            ))
        }
    }

    #[test]
    fn uf2_has_rp2040_family_and_expected_blocks() {
        let image = vec![0xA5; 300];
        let uf2 = encode_uf2(&image);
        assert_eq!(uf2.len(), UF2_BLOCK_SIZE * 2);
        assert_eq!(
            u32::from_le_bytes(uf2[0..4].try_into().unwrap()),
            UF2_MAGIC_START0
        );
        assert_eq!(
            u32::from_le_bytes(uf2[28..32].try_into().unwrap()),
            RP2040_FAMILY_ID
        );
        assert_eq!(
            u32::from_le_bytes(uf2[12..16].try_into().unwrap()),
            RP2040_UF2_BASE_ADDRESS
        );
        assert_eq!(
            u32::from_le_bytes(uf2[512 + 12..512 + 16].try_into().unwrap()),
            RP2040_UF2_BASE_ADDRESS + UF2_PAYLOAD_SIZE as u32
        );
        assert_eq!(
            u32::from_le_bytes(uf2[512 + 20..512 + 24].try_into().unwrap()),
            1
        );
        assert_eq!(
            u32::from_le_bytes(uf2[512 + 16..512 + 20].try_into().unwrap()),
            UF2_PAYLOAD_SIZE as u32
        );
        assert_eq!(&uf2[32..32 + 256], &image[..256]);
        assert_eq!(&uf2[512 + 32..512 + 32 + 44], &image[256..]);
        assert_eq!(
            u32::from_le_bytes(uf2[508..512].try_into().unwrap()),
            UF2_MAGIC_END
        );
    }

    #[test]
    fn uf2_magic_matches_the_published_byte_sequence() {
        assert_eq!(UF2_MAGIC_START0.to_le_bytes(), [0x55, 0x46, 0x32, 0x0A]);
        assert_eq!(UF2_MAGIC_START1.to_le_bytes(), [0x57, 0x51, 0x5D, 0x9E]);
        assert_eq!(UF2_MAGIC_END.to_le_bytes(), [0x30, 0x6F, 0xB1, 0x0A]);
    }

    #[test]
    fn full_image_boot2_uses_xip_address() {
        // Prefix emitted by the RP2040 second-stage bootloader in FastLED's
        // complete firmware.bin artifact.
        let boot2_prefix = [0x00, 0xB5, 0x32, 0x4B, 0x21, 0x20, 0x58, 0x60];
        let uf2 = encode_uf2(&boot2_prefix);
        assert_eq!(
            u32::from_le_bytes(uf2[12..16].try_into().unwrap()),
            0x1000_0000
        );
        assert_eq!(&uf2[32..40], &boot2_prefix);
    }

    #[test]
    fn finds_marker_volume_without_requiring_a_drive_letter() {
        let root = tempdir().unwrap();
        let volume = root.path().join("RPI-RP2");
        fs::create_dir(&volume).unwrap();
        fs::write(volume.join("INFO_UF2.TXT"), "Model: Raspberry Pi RP2").unwrap();
        assert_eq!(find_uf2_volume(&[root.path().to_path_buf()]), Some(volume));
    }

    #[test]
    fn host_volume_roots_cover_windows_linux_and_macos_without_host_dependency() {
        let windows = volume_roots_for(true, None);
        assert_eq!(windows.len(), 26);
        assert_eq!(windows.first(), Some(&PathBuf::from("A:\\")));
        assert_eq!(windows.last(), Some(&PathBuf::from("Z:\\")));

        let unix = volume_roots_for(false, Some(Path::new("/home/alice")));
        assert!(unix.contains(&PathBuf::from("/media/alice")));
        assert!(unix.contains(&PathBuf::from("/run/media/alice")));
        assert!(unix.contains(&PathBuf::from("/Volumes")));
        assert!(unix.contains(&PathBuf::from("/media")));
        assert!(unix.contains(&PathBuf::from("/run/media")));
    }

    #[test]
    fn windows_roots_keep_only_predicate_approved_letters() {
        let roots = volume_roots_filtered(true, None, |root| {
            root == Path::new("D:\\") || root == Path::new("E:\\")
        });
        assert_eq!(roots, vec![PathBuf::from("D:\\"), PathBuf::from("E:\\")]);
    }

    #[test]
    fn windows_roots_are_empty_when_predicate_rejects_every_letter() {
        let roots = volume_roots_filtered(true, None, |_| false);
        assert!(roots.is_empty());
    }

    #[test]
    fn non_windows_roots_ignore_the_injected_predicate() {
        let filtered = volume_roots_filtered(false, Some(Path::new("/home/alice")), |_| false);
        let unfiltered = volume_roots_filtered(false, Some(Path::new("/home/alice")), |_| true);
        assert_eq!(filtered, unfiltered);
        assert!(filtered.contains(&PathBuf::from("/media/alice")));
        assert!(filtered.contains(&PathBuf::from("/Volumes")));
    }

    #[test]
    fn with_topology_appends_known_summary() {
        assert_eq!(
            with_topology("boom".to_string(), Some("USB topology: direct root port")),
            "boom. USB topology: direct root port"
        );
    }

    #[test]
    fn with_topology_falls_back_when_capture_failed() {
        assert_eq!(
            with_topology("boom".to_string(), None),
            "boom. USB topology unavailable"
        );
    }

    #[test]
    fn zero_and_multiple_bootsel_volumes_fail_safely() {
        assert_eq!(select_single_uf2_volume(Vec::new()).unwrap(), None);
        let error = select_single_uf2_volume(vec![
            PathBuf::from("first-rpi-rp2"),
            PathBuf::from("second-rpi-rp2"),
        ])
        .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("multiple RP2040 BOOTSEL volumes")
        );
    }

    #[test]
    fn appeared_volume_attribution_ignores_stale_volumes() {
        let before = BTreeSet::from([PathBuf::from("stale-1"), PathBuf::from("stale-2")]);
        assert_eq!(
            select_appeared_volume(
                &before,
                vec![
                    PathBuf::from("stale-1"),
                    PathBuf::from("stale-2"),
                    PathBuf::from("fresh"),
                ],
            )
            .unwrap(),
            Some(PathBuf::from("fresh"))
        );
        // Nothing new yet: keep polling rather than grabbing a stale volume.
        assert_eq!(
            select_appeared_volume(&before, vec![PathBuf::from("stale-1")]).unwrap(),
            None
        );
        let error = select_appeared_volume(
            &before,
            vec![PathBuf::from("fresh-1"), PathBuf::from("fresh-2")],
        )
        .unwrap_err();
        let message = error.to_string();
        assert!(message.contains("multiple RP2040 BOOTSEL volumes"));
        assert!(message.contains("fresh-1"));
        assert!(message.contains("fresh-2"));
    }

    #[test]
    fn empty_before_set_reduces_attribution_to_single_volume_selection() {
        let before = BTreeSet::new();
        assert_eq!(select_appeared_volume(&before, Vec::new()).unwrap(), None);
        assert_eq!(
            select_appeared_volume(&before, vec![PathBuf::from("only")]).unwrap(),
            Some(PathBuf::from("only"))
        );
        let error = select_appeared_volume(&before, vec![PathBuf::from("a"), PathBuf::from("b")])
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("multiple RP2040 BOOTSEL volumes")
        );
    }

    #[test]
    fn pretouch_policy_keeps_single_volume_and_gates_multi_volume_on_attribution() {
        assert_eq!(
            pretouch_volume_policy(Vec::new(), false).unwrap(),
            (None, BTreeSet::new())
        );
        // A single mounted volume wins outright, selector or not (#1040).
        for can_attribute in [true, false] {
            assert_eq!(
                pretouch_volume_policy(vec![PathBuf::from("only")], can_attribute).unwrap(),
                (Some(PathBuf::from("only")), BTreeSet::new())
            );
        }
        let (volume, before) =
            pretouch_volume_policy(vec![PathBuf::from("a"), PathBuf::from("b")], true).unwrap();
        assert_eq!(volume, None);
        assert_eq!(
            before,
            BTreeSet::from([PathBuf::from("a"), PathBuf::from("b")])
        );
        let error = pretouch_volume_policy(vec![PathBuf::from("a"), PathBuf::from("b")], false)
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("multiple RP2040 BOOTSEL volumes")
        );
    }

    #[test]
    fn bootloader_watcher_attributes_the_volume_that_appears_after_the_touch() {
        let stale = BTreeSet::from([PathBuf::from("stale")]);
        let mut scans = 0;
        let found = find_uf2_volume_until_with(
            Duration::from_secs(1),
            || {
                scans += 1;
                let discovered = if scans == 1 {
                    vec![PathBuf::from("stale")]
                } else {
                    vec![PathBuf::from("stale"), PathBuf::from("fresh")]
                };
                select_appeared_volume(&stale, discovered)
            },
            || false,
        )
        .unwrap();
        assert_eq!(found, Some(PathBuf::from("fresh")));
        assert!(scans >= 2);
    }

    #[test]
    fn bootsel_not_found_message_adds_macos_accessory_hint_only_on_macos() {
        let plain = bootsel_not_found_message(false);
        assert!(plain.contains("BOOTSEL volume not found"));
        assert!(plain.contains(BOOTLOADER_TIMEOUT_ENV));
        assert!(!plain.contains("Accessories"));

        let macos = bootsel_not_found_message(true);
        assert!(macos.starts_with(&plain));
        assert!(macos.contains("Allow accessory to connect?"));
        assert!(macos.contains("Privacy & Security > Accessories"));
    }

    #[test]
    fn windows_5_write_denial_points_at_host_policy_not_the_board() {
        let error = io::Error::from_raw_os_error(5);
        let message =
            format_uf2_copy_error(Path::new("firmware.uf2"), Path::new("G:/NEW.UF2"), &error);
        assert!(message.contains("error 5"));
        assert!(message.contains("Removable Disks: Deny write access"));
        assert!(message.contains("FDVDenyWriteAccess"));
        assert!(message.contains("not a board fault"));
        assert!(message.contains("administrator"));
        assert!(message.contains("will not clear a policy block"));
    }

    #[test]
    fn bootloader_watcher_timeout_is_deterministic() {
        let mut scans = 0;
        let mut mounts = 0;
        let found = find_uf2_volume_until_with(
            Duration::ZERO,
            || {
                scans += 1;
                Ok(None)
            },
            || {
                mounts += 1;
                false
            },
        )
        .unwrap();
        assert_eq!(found, None);
        assert_eq!(scans, 1);
        assert_eq!(mounts, 1);
    }

    #[test]
    fn reset_disconnect_error_is_accepted_only_after_bootsel_appears() {
        let volume = PathBuf::from("G:/");
        assert_eq!(
            select_volume_after_reset(Some(volume.clone()), None).unwrap(),
            volume
        );

        let reset_error = FbuildError::SerialError(
            "failed to set the RP2040 reset baud on COM12: device disappeared".to_string(),
        );

        assert_eq!(
            select_volume_after_reset(Some(volume.clone()), Some(reset_error)).unwrap(),
            volume
        );

        let reset_error = FbuildError::SerialError(
            "failed to set the RP2040 reset baud on COM12: device disappeared".to_string(),
        );
        let error = select_volume_after_reset(None, Some(reset_error)).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("failed to set the RP2040 reset baud")
        );
        assert!(error.to_string().contains("no RP2040 BOOTSEL transition"));
        assert!(error.to_string().contains(BOOTLOADER_TIMEOUT_ENV));

        let error = select_volume_after_reset(None, None).unwrap_err();
        assert!(error.to_string().contains("BOOTSEL volume not found"));
        assert!(error.to_string().contains(BOOTLOADER_TIMEOUT_ENV));
    }

    #[test]
    fn overlapping_linux_roots_do_not_duplicate_one_volume() {
        let root = tempdir().unwrap();
        let user_root = root.path().join("media").join("alice");
        let volume = user_root.join("RPI-RP2");
        fs::create_dir_all(&volume).unwrap();
        fs::write(volume.join("INFO_UF2.TXT"), "Model: Raspberry Pi RP2").unwrap();
        assert_eq!(
            find_uf2_volumes(&[user_root, root.path().join("media")]),
            vec![volume]
        );
    }

    #[test]
    fn writes_bin_as_uf2_to_marker_volume() {
        let root = tempdir().unwrap();
        fs::write(root.path().join("INFO_UF2.TXT"), "UF2 Bootloader").unwrap();
        let firmware = root.path().join("firmware.bin");
        fs::write(&firmware, [1u8, 2, 3]).unwrap();
        let destination = write_uf2(&firmware, root.path(), RP2040_FAMILY_ID).unwrap();
        assert_eq!(destination.file_name().unwrap(), "NEW.UF2");
        let artifact = firmware.with_extension("uf2");
        assert!(artifact.is_file());
        assert_eq!(
            fs::read(&artifact).unwrap(),
            fs::read(&destination).unwrap()
        );
        assert_eq!(
            fs::metadata(destination).unwrap().len(),
            UF2_BLOCK_SIZE as u64
        );
    }

    #[test]
    fn rejects_wrong_family_and_malformed_uf2() {
        let wrong = encode_uf2_for_family(&[1, 2, 3], RP2350_FAMILY_ID);
        let error = validate_uf2(&wrong, RP2040_FAMILY_ID).unwrap_err();
        assert!(error.to_string().contains("wrong UF2 family"));

        let error = validate_uf2(&[0; 511], RP2040_FAMILY_ID).unwrap_err();
        assert!(error.to_string().contains("malformed RP2040 UF2"));

        let mut duplicate = encode_uf2(&[0; 300]);
        duplicate[512 + 20..512 + 24].copy_from_slice(&0u32.to_le_bytes());
        let error = validate_uf2(&duplicate, RP2040_FAMILY_ID).unwrap_err();
        assert!(error.to_string().contains("duplicate block number"));

        let mut bad_address = encode_uf2(&[0; 1]);
        bad_address[12..16].copy_from_slice(&0x3000_0000u32.to_le_bytes());
        let error = validate_uf2(&bad_address, RP2040_FAMILY_ID).unwrap_err();
        assert!(error.to_string().contains("block metadata"));

        let mut overlapping = encode_uf2(&[0; 300]);
        overlapping[512 + 12..512 + 16].copy_from_slice(&RP2040_UF2_BASE_ADDRESS.to_le_bytes());
        let error = validate_uf2(&overlapping, RP2040_FAMILY_ID).unwrap_err();
        assert!(error.to_string().contains("overlapping target address"));
    }

    #[test]
    fn accepts_and_classifies_rp2040_ram_uf2() {
        for target_address in [
            RP2040_SRAM_START,
            RP2040_SRAM_END - UF2_PAYLOAD_SIZE as u32,
            RP2040_XIP_SRAM_START + 1,
            RP2040_XIP_SRAM_END - UF2_PAYLOAD_SIZE as u32,
        ] {
            let mut ram = encode_uf2(&[0xFE, 0xE7]);
            ram[12..16].copy_from_slice(&target_address.to_le_bytes());
            assert_eq!(
                validate_uf2(&ram, RP2040_FAMILY_ID).unwrap(),
                Uf2Target::Ram
            );
        }

        assert_eq!(
            validate_uf2(&encode_uf2(&[1, 2, 3]), RP2040_FAMILY_ID).unwrap(),
            Uf2Target::Flash
        );
    }

    #[test]
    fn prepared_artifact_preserves_ram_classification_and_bin_stays_flash() {
        let root = tempdir().unwrap();
        let ram_path = root.path().join("probe.uf2");
        let mut ram = encode_uf2(&[0xFE, 0xE7]);
        ram[12..16].copy_from_slice(&RP2040_SRAM_START.to_le_bytes());
        fs::write(&ram_path, ram).unwrap();
        let (prepared, target) = prepare_uf2_artifact(&ram_path, RP2040_FAMILY_ID).unwrap();
        assert_eq!(prepared, ram_path);
        assert_eq!(target, Uf2Target::Ram);

        let bin_path = root.path().join("firmware.bin");
        fs::write(&bin_path, [1, 2, 3]).unwrap();
        let (_, target) = prepare_uf2_artifact(&bin_path, RP2040_FAMILY_ID).unwrap();
        assert_eq!(target, Uf2Target::Flash);
    }

    #[test]
    fn ram_load_result_is_success_without_claiming_a_runtime_port() {
        let result = ram_load_result(
            Some(Path::new("G:/")),
            "BOOTSEL mass-storage",
            "wrote G:/NEW.UF2".to_string(),
            String::new(),
        );
        assert!(result.success);
        assert_eq!(result.port, None);
        assert!(result.message.contains("accepted for execution"));
        assert!(matches!(result.outcome, DeployOutcome::RamLoad));
    }

    #[test]
    fn rejects_invalid_or_mixed_rp2040_ram_uf2() {
        for target_address in [
            RP2040_XIP_SRAM_END - UF2_PAYLOAD_SIZE as u32 + 1,
            RP2040_SRAM_END - UF2_PAYLOAD_SIZE as u32 + 1,
            RP2040_SRAM_END,
        ] {
            let mut invalid = encode_uf2(&[0xFE, 0xE7]);
            invalid[12..16].copy_from_slice(&target_address.to_le_bytes());
            let error = validate_uf2(&invalid, RP2040_FAMILY_ID).unwrap_err();
            assert!(error.to_string().contains("block metadata"));
        }

        let mut mixed = encode_uf2(&[0; UF2_PAYLOAD_SIZE + 1]);
        mixed[512 + 12..512 + 16].copy_from_slice(&RP2040_SRAM_START.to_le_bytes());
        let error = validate_uf2(&mixed, RP2040_FAMILY_ID).unwrap_err();
        assert!(error.to_string().contains("mixes flash and RAM"));

        let mut rp2350_ram = encode_uf2_for_family(&[0xFE, 0xE7], RP2350_FAMILY_ID);
        rp2350_ram[12..16].copy_from_slice(&RP2040_SRAM_START.to_le_bytes());
        let error = validate_uf2(&rp2350_ram, RP2350_FAMILY_ID).unwrap_err();
        assert!(error.to_string().contains("block metadata"));
    }

    #[test]
    fn rejects_overlapping_unaligned_ram_pages() {
        let mut overlapping = encode_uf2(&[0; UF2_PAYLOAD_SIZE + 1]);
        overlapping[12..16].copy_from_slice(&(RP2040_SRAM_START + 1).to_le_bytes());
        overlapping[512 + 12..512 + 16].copy_from_slice(&(RP2040_SRAM_START + 128).to_le_bytes());
        let error = validate_uf2(&overlapping, RP2040_FAMILY_ID).unwrap_err();
        assert!(error.to_string().contains("overlapping target address"));
    }

    #[test]
    fn refuses_to_encode_elf_or_unknown_input_as_raw_flash() {
        let root = tempdir().unwrap();
        fs::write(root.path().join("INFO_UF2.TXT"), "UF2 Bootloader").unwrap();
        for filename in ["firmware.elf", "firmware.dat"] {
            let firmware = root.path().join(filename);
            fs::write(&firmware, [0x7f, b'E', b'L', b'F']).unwrap();
            let error = write_uf2(&firmware, root.path(), RP2040_FAMILY_ID).unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains("expected a managed .uf2 or raw .bin")
            );
        }
    }

    #[test]
    fn already_mounted_explicit_volume_wins_without_serial_selection() {
        let root = tempdir().unwrap();
        fs::write(root.path().join("INFO_UF2.TXT"), "Board-ID: RPI-RP2").unwrap();
        let selector = format!("UF2={}", root.path().display());
        assert_eq!(
            explicit_uf2_volume(&selector),
            Some(root.path().to_path_buf())
        );
        assert!(resolve_requested_runtime_target(&selector, &[]).is_err());
    }

    #[test]
    fn deploy_family_comes_from_mcu_not_board_name() {
        assert_eq!(
            Rp2040Deployer::for_mcu("rp2040").unwrap().family_id,
            RP2040_FAMILY_ID
        );
        assert_eq!(
            Rp2040Deployer::for_mcu("rp2350").unwrap().family_id,
            RP2350_FAMILY_ID
        );
    }

    #[test]
    fn typed_profile_must_match_runtime_role_interface_and_generation() {
        use fbuild_core::usb::profiles::{
            UsbDeviceRole, UsbIdentityMatch, UsbProfileProvenance, UsbPurpose, UsbTransportProfile,
        };

        let mut profile = UsbTransportProfile {
            identity_match: UsbIdentityMatch {
                vid: "feed".to_string(),
                pid: Some("c0de".to_string()),
                pid_mask: None,
            },
            purpose: UsbPurpose::Runtime,
            role: UsbDeviceRole::RuntimeCdc,
            transport: "usb".to_string(),
            reset: "touch-1200".to_string(),
            handoff: "bootloader".to_string(),
            platform: Some("synthetic".to_string()),
            family: Some("rp2040".to_string()),
            generation: Some("synthetic".to_string()),
            interface: Some("cdc".to_string()),
            provenance: UsbProfileProvenance {
                source_url: "test://fixture".to_string(),
                source_revision: "a".repeat(40),
                source_class: "test".to_string(),
            },
            priority: 100,
            allow_ambiguous: false,
        };
        assert!(profile_matches_family(&profile, RP2040_FAMILY_ID));
        assert!(!profile_matches_family(&profile, RP2350_FAMILY_ID));
        profile.family = Some("rp2350".to_string());
        assert!(profile_matches_family(&profile, RP2350_FAMILY_ID));
        profile.role = UsbDeviceRole::BootloaderUf2;
        assert!(!profile_matches_family(&profile, RP2350_FAMILY_ID));
    }

    #[test]
    fn successful_rom_transfer_waits_for_marker_disappearance() {
        let volume = tempdir().unwrap();
        let marker = volume.path().join("INFO_UF2.TXT");
        fs::write(&marker, "Model: Raspberry Pi RP2").unwrap();
        let marker_for_thread = marker.clone();
        let remover = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(25));
            fs::remove_file(marker_for_thread).unwrap();
        });
        wait_for_volume_disappearance(volume.path(), Duration::from_secs(1)).unwrap();
        remover.join().unwrap();
    }

    #[test]
    fn volume_disappearance_timeout_is_actionable_without_diagnosing_qspi() {
        let volume = tempdir().unwrap();
        fs::write(
            volume.path().join("INFO_UF2.TXT"),
            "Model: Raspberry Pi RP2",
        )
        .unwrap();
        let error = wait_for_volume_disappearance(volume.path(), Duration::ZERO).unwrap_err();
        let message = error.to_string();
        assert!(message.contains("did not eject after NEW.UF2"));
        assert!(message.contains("deployment is unconfirmed"));
        assert!(message.contains("does not identify a QSPI flash fault"));
    }

    fn accept_probe(_port: &PicoCdcPort) -> std::result::Result<(), String> {
        Ok(())
    }

    fn cdc_candidate(
        name: &str,
        serial: Option<&str>,
        health: fbuild_serial::ports::PortHealth,
    ) -> PicoCdcPort {
        PicoCdcPort {
            name: name.to_string(),
            serial_number: serial.map(str::to_string),
            health,
            instance_id: Some(format!("USB\\VID_2E8A&PID_000A\\{name}")),
            parent_instance_id: None,
        }
    }

    #[test]
    fn cdc_timeout_without_flash_confirmation_is_an_actionable_failure() {
        let wait = wait_for_cdc_port_with_clock(
            Some("COM7"),
            None,
            &BTreeSet::from(["COM7".to_string()]),
            Duration::ZERO,
            || Ok(Vec::new()),
            accept_probe,
            || Duration::ZERO,
            |_| {},
        );
        assert!(matches!(wait, Err(CdcWaitError::Timeout(_))));
        let error = resolve_post_flash_cdc(false, wait, Duration::from_secs(15)).unwrap_err();
        let message = error.to_string();
        assert!(message.contains("firmware was transferred"));
        assert!(message.contains("no catalogue-identified runtime CDC port appeared"));
        assert!(message.contains(POST_DEPLOY_TIMEOUT_ENV));
    }

    #[test]
    fn cdc_timeout_after_confirmed_flash_downgrades_to_unconfirmed_success() {
        let outcome = resolve_post_flash_cdc(
            true,
            Err(CdcWaitError::Timeout(CdcWaitTimeout {
                elapsed: Duration::from_secs(15),
                previous_port: Some("COM7".to_string()),
                requested_serial: Some("serial".to_string()),
                candidates: Vec::new(),
                last_open_error: None,
            })),
            Duration::from_secs(15),
        )
        .unwrap();
        let PostFlashCdc::Unconfirmed(note) = outcome else {
            panic!("expected an unconfirmed-CDC downgrade, got {outcome:?}");
        };
        assert!(note.contains("flashed and accepted"));
        assert!(note.contains("reappeared within 15s"));
        assert!(note.contains("irst-plug driver installation"));
        assert!(note.contains(POST_DEPLOY_TIMEOUT_ENV));
        assert!(note.contains("hold BOOT/BOOTSEL"));
    }

    #[test]
    fn confirmed_flash_with_phantom_history_reports_recovery_guidance() {
        let outcome = resolve_post_flash_cdc(
            true,
            Err(CdcWaitError::Timeout(CdcWaitTimeout {
                elapsed: Duration::from_secs(30),
                previous_port: Some("COM12".to_string()),
                requested_serial: Some("5303284720C4641C".to_string()),
                candidates: vec![cdc_candidate(
                    "COM12",
                    Some("5303284720C4641C"),
                    fbuild_serial::ports::PortHealth::Phantom {
                        problem_code: Some(45),
                        status: None,
                    },
                )],
                last_open_error: Some("COM12: Access is denied.".to_string()),
            })),
            Duration::from_secs(30),
        )
        .unwrap();
        let PostFlashCdc::Unconfirmed(note) = outcome else {
            panic!("expected an unconfirmed-CDC downgrade, got {outcome:?}");
        };
        assert!(note.contains("health phantom"), "missing health: {note}");
        assert!(note.contains("problem code 45"), "missing code: {note}");
        assert!(
            note.contains("USB\\VID_2E8A&PID_000A\\COM12"),
            "missing instance: {note}"
        );
        assert!(
            note.contains("last open error COM12: Access is denied."),
            "missing open error: {note}"
        );
        assert!(
            note.contains("not a live endpoint"),
            "missing phantom explanation: {note}"
        );
        assert!(note.contains("RPI-RP2"), "missing BOOTSEL steps: {note}");
    }

    #[test]
    fn open_probe_failure_is_never_returned_and_lands_in_diagnostics() {
        let mut probes = 0;
        let wait = wait_for_cdc_port_with_clock(
            None,
            Some("5303284720C4641C"),
            &BTreeSet::new(),
            Duration::from_secs(1),
            || {
                Ok(vec![cdc_candidate(
                    "COM27",
                    Some("5303284720C4641C"),
                    fbuild_serial::ports::PortHealth::HealthyPresent,
                )])
            },
            |port: &PicoCdcPort| {
                probes += 1;
                Err(format!("open {} timed out", port.name))
            },
            || Duration::from_secs(2),
            |_| {},
        );
        let Err(CdcWaitError::Timeout(timeout)) = wait else {
            panic!("an unopenable candidate must never be returned, got {wait:?}");
        };
        assert!(probes >= 1);
        assert_eq!(
            timeout.last_open_error.as_deref(),
            Some("COM27: open COM27 timed out")
        );
        assert!(timeout.diagnostics().contains("last open error COM27"));
    }

    #[test]
    fn candidate_that_turns_phantom_is_never_returned() {
        let mut scans = 0;
        let wait = wait_for_cdc_port_with_clock(
            Some("COM12"),
            Some("5303284720C4641C"),
            &BTreeSet::from(["COM12".to_string()]),
            Duration::from_secs(1),
            || {
                scans += 1;
                Ok(vec![cdc_candidate(
                    "COM12",
                    Some("5303284720C4641C"),
                    if scans == 1 {
                        fbuild_serial::ports::PortHealth::Phantom {
                            problem_code: None,
                            status: None,
                        }
                    } else {
                        fbuild_serial::ports::PortHealth::Phantom {
                            problem_code: Some(45),
                            status: None,
                        }
                    },
                )])
            },
            |_port: &PicoCdcPort| panic!("a phantom record must never reach the open probe"),
            || Duration::from_secs(2),
            |_| {},
        );
        assert!(matches!(wait, Err(CdcWaitError::Timeout(_))));
    }

    #[test]
    fn cdc_timeout_diagnostics_identify_the_prior_target_and_catalogue_candidates() {
        let diagnostics = CdcWaitTimeout {
            elapsed: Duration::from_secs(30),
            previous_port: Some("COM12".to_string()),
            requested_serial: Some("5303284720C4641C".to_string()),
            candidates: vec![PicoCdcPort {
                name: "COM27".to_string(),
                serial_number: Some("5303284720C4641C".to_string()),
                health: fbuild_serial::ports::PortHealth::Unknown,
                instance_id: None,
                parent_instance_id: None,
            }],
            last_open_error: None,
        }
        .diagnostics();

        assert!(diagnostics.contains("elapsed 30000ms"));
        assert!(diagnostics.contains("prior port COM12"));
        assert!(diagnostics.contains("requested serial 5303284720C4641C"));
        assert!(diagnostics.contains("COM27 (serial 5303284720C4641C; health unknown)"));
    }

    #[test]
    fn cdc_enumeration_error_fails_even_after_confirmed_flash() {
        let wait = wait_for_cdc_port_with_clock(
            None,
            None,
            &BTreeSet::new(),
            Duration::from_secs(5),
            || Err(FbuildError::SerialError("enumeration exploded".into())),
            accept_probe,
            || Duration::ZERO,
            |_| {},
        );
        let error = resolve_post_flash_cdc(true, wait, Duration::from_secs(15)).unwrap_err();
        assert!(error.to_string().contains("enumeration exploded"));
    }

    #[test]
    fn cdc_port_found_is_confirmed_regardless_of_flash_state() {
        for flash_confirmed in [true, false] {
            let outcome = resolve_post_flash_cdc(
                flash_confirmed,
                Ok(cdc_candidate(
                    "COM9",
                    None,
                    fbuild_serial::ports::PortHealth::HealthyPresent,
                )),
                Duration::from_secs(15),
            )
            .unwrap();
            assert!(matches!(outcome, PostFlashCdc::Confirmed(port) if port == "COM9"));
        }
    }

    #[test]
    fn delayed_matching_cdc_after_the_legacy_window_returns_its_new_port() {
        let mut scans = 0;
        let mut elapsed = [Duration::from_secs(16)].into_iter();
        let port = wait_for_cdc_port_with_clock(
            Some("COM12"),
            Some("5303284720C4641C"),
            &BTreeSet::from(["COM12".to_string()]),
            Duration::from_secs(30),
            || {
                scans += 1;
                Ok(if scans == 1 {
                    Vec::new()
                } else {
                    vec![PicoCdcPort {
                        name: "COM27".to_string(),
                        serial_number: Some("5303284720C4641C".to_string()),
                        health: fbuild_serial::ports::PortHealth::Unknown,
                        instance_id: None,
                        parent_instance_id: None,
                    }]
                })
            },
            accept_probe,
            || elapsed.next().unwrap_or(Duration::from_secs(16)),
            |_| {},
        )
        .expect("a matching delayed Pico CDC endpoint must be returned");

        assert_eq!(port.name, "COM27");
        assert_eq!(scans, 2);
    }

    #[test]
    fn final_deadline_catalogue_scan_accepts_a_matching_renumbered_port() {
        let mut scans = 0;
        let mut elapsed = [Duration::from_secs(30)].into_iter();
        let port = wait_for_cdc_port_with_clock(
            Some("COM12"),
            Some("5303284720C4641C"),
            &BTreeSet::from(["COM12".to_string()]),
            Duration::from_secs(30),
            || {
                scans += 1;
                Ok(vec![PicoCdcPort {
                    name: "COM27".to_string(),
                    serial_number: Some("5303284720C4641C".to_string()),
                    health: fbuild_serial::ports::PortHealth::Unknown,
                    instance_id: None,
                    parent_instance_id: None,
                }])
            },
            accept_probe,
            || elapsed.next().unwrap_or(Duration::from_secs(30)),
            |_| {},
        )
        .expect("the final catalogue scan must accept a boundary arrival");

        assert_eq!(port.name, "COM27");
        assert_eq!(scans, 1);
    }

    #[test]
    fn rp2040_post_flash_cdc_policy_exceeds_the_legacy_15_second_window() {
        assert_eq!(rp2040_post_deploy_timeout(), Duration::from_secs(30));
    }

    #[test]
    fn ambiguous_cdc_selection_is_an_enumeration_error_not_a_timeout() {
        let wait = wait_for_cdc_port_with_clock(
            None,
            None,
            &BTreeSet::new(),
            Duration::ZERO,
            || {
                Ok(vec![
                    PicoCdcPort {
                        name: "COM12".to_string(),
                        serial_number: None,
                        health: fbuild_serial::ports::PortHealth::Unknown,
                        instance_id: None,
                        parent_instance_id: None,
                    },
                    PicoCdcPort {
                        name: "COM13".to_string(),
                        serial_number: None,
                        health: fbuild_serial::ports::PortHealth::Unknown,
                        instance_id: None,
                        parent_instance_id: None,
                    },
                ])
            },
            accept_probe,
            || Duration::ZERO,
            |_| {},
        );
        let error = resolve_post_flash_cdc(true, wait, Duration::from_secs(15)).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("multiple new Raspberry Pi CDC ports")
        );
    }

    #[test]
    fn timeout_override_accepts_integer_seconds_in_range() {
        let default = Duration::from_secs(10);
        assert_eq!(
            timeout_secs_from(Some("30"), default, "VAR"),
            Duration::from_secs(30)
        );
        assert_eq!(
            timeout_secs_from(Some("1"), default, "VAR"),
            Duration::from_secs(1)
        );
        assert_eq!(
            timeout_secs_from(Some("600"), default, "VAR"),
            Duration::from_secs(600)
        );
        assert_eq!(
            timeout_secs_from(Some(" 45 "), default, "VAR"),
            Duration::from_secs(45)
        );
    }

    #[test]
    fn timeout_override_falls_back_on_out_of_range_or_garbage() {
        let default = Duration::from_secs(10);
        for raw in [
            None,
            Some("0"),
            Some("601"),
            Some("-5"),
            Some("abc"),
            Some(""),
            Some("1.5"),
        ] {
            assert_eq!(timeout_secs_from(raw, default, "VAR"), default);
        }
    }

    #[test]
    fn watchdog_passes_through_fast_results_and_errors() {
        assert_eq!(
            run_with_watchdog(Duration::from_secs(5), "probe", || Ok(7)).unwrap(),
            7
        );
        let error = run_with_watchdog(Duration::from_secs(5), "probe", || -> Result<()> {
            Err(FbuildError::DeployFailed("inner failure".into()))
        })
        .unwrap_err();
        assert!(error.to_string().contains("inner failure"));
    }

    #[test]
    fn watchdog_timeout_reports_a_live_abandoned_worker() {
        let (release, wait_for_release) = std::sync::mpsc::channel();
        let started = Instant::now();
        let error = run_with_watchdog(Duration::from_millis(50), "RP2040 UF2 write", move || {
            wait_for_release.recv().unwrap();
            Ok(())
        })
        .unwrap_err();
        assert!(started.elapsed() < Duration::from_secs(1));
        let message = error.to_string();
        assert!(message.contains("RP2040 UF2 write did not complete within"));
        assert!(message.contains(UF2_WRITE_TIMEOUT_ENV));
        assert!(message.contains("abandoned worker"));

        release.send(()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(1);
        while watchdog_diagnostics().contains("abandoned worker(s)") && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(watchdog_diagnostics().contains("no abandoned workers"));
    }

    #[test]
    fn copy_error_is_accepted_only_when_rom_volume_disappears() {
        let root = tempdir().unwrap();
        let marker = root.path().join("INFO_UF2.TXT");
        let artifact = root.path().join("firmware.uf2");
        let destination = root.path().join("NEW.UF2");
        fs::write(&marker, "Model: Raspberry Pi RP2").unwrap();
        fs::write(&artifact, encode_uf2(&[1, 2, 3])).unwrap();

        let artifact_len = fs::metadata(&artifact).unwrap().len();
        copy_uf2_artifact_with(&artifact, &destination, root.path(), |_, _| {
            fs::remove_file(&marker).unwrap();
            Err(Uf2WriteFailure::new(
                std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "device disappeared after final block",
                ),
                artifact_len,
            ))
        })
        .unwrap();

        fs::write(&marker, "Model: Raspberry Pi RP2").unwrap();
        let error = copy_uf2_artifact_with(&artifact, &destination, root.path(), |_, _| {
            Err(Uf2WriteFailure::new(
                std::io::Error::other("corrupt volume"),
                artifact_len,
            ))
        })
        .unwrap_err();
        assert!(error.to_string().contains("corrupt volume"));

        fs::remove_file(&marker).unwrap();
        let error = copy_uf2_artifact_with(&artifact, &destination, root.path(), |_, _| {
            Err(Uf2WriteFailure::new(
                std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "source permission denied",
                ),
                0,
            ))
        })
        .unwrap_err();
        assert!(error.to_string().contains("source permission denied"));
    }

    #[test]
    fn direct_uf2_write_preserves_bytes_and_truncates_destination() {
        let root = tempdir().unwrap();
        let artifact = root.path().join("firmware.uf2");
        let destination = root.path().join("NEW.UF2");
        let bytes = vec![0xA5; 128 * 1024 + UF2_BLOCK_SIZE];
        fs::write(&artifact, &bytes).unwrap();
        fs::write(&destination, vec![0xCC; bytes.len() * 2]).unwrap();

        let written = write_uf2_artifact_direct(&artifact, &destination).unwrap();

        assert_eq!(written, bytes.len() as u64);
        assert_eq!(fs::read(destination).unwrap(), bytes);
    }

    #[cfg(windows)]
    #[test]
    fn uf2_destination_matches_ucrt_delete_sharing() {
        use std::os::windows::fs::OpenOptionsExt;

        const DELETE_ACCESS: u32 = 0x0001_0000;
        const ERROR_SHARING_VIOLATION: i32 = 32;

        let root = tempdir().unwrap();
        let destination = root.path().join("NEW.UF2");
        let _writer = open_uf2_destination(&destination).unwrap();

        let error = fs::OpenOptions::new()
            .access_mode(DELETE_ACCESS)
            .open(&destination)
            .unwrap_err();
        assert_eq!(error.raw_os_error(), Some(ERROR_SHARING_VIOLATION));
    }

    #[test]
    fn whole_buffer_writer_tracks_short_writes_and_flush_failures() {
        let bytes = vec![0xA5; 4096];
        assert_eq!(
            write_uf2_bytes(ShortWriter { max_write: 17 }, &bytes).unwrap(),
            bytes.len() as u64
        );

        let failure = write_uf2_bytes(FlushFailureWriter, &bytes).unwrap_err();
        assert_eq!(failure.bytes_written, bytes.len() as u64);
        assert_eq!(failure.error.kind(), io::ErrorKind::NotFound);
    }

    #[test]
    fn eject_error_before_all_bytes_are_written_is_rejected() {
        let root = tempdir().unwrap();
        let marker = root.path().join("INFO_UF2.TXT");
        let artifact = root.path().join("firmware.uf2");
        let destination = root.path().join("NEW.UF2");
        fs::write(&marker, "Model: Raspberry Pi RP2").unwrap();
        fs::write(&artifact, encode_uf2(&[1, 2, 3])).unwrap();
        fs::remove_file(&marker).unwrap();

        let error = copy_uf2_artifact_with(&artifact, &destination, root.path(), |_, _| {
            Err(Uf2WriteFailure::new(
                std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "device disappeared during transfer",
                ),
                0,
            ))
        })
        .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("device disappeared during transfer")
        );
    }

    #[test]
    fn windows_1392_copy_error_warns_against_repairing_synthetic_volume() {
        let error = std::io::Error::from_raw_os_error(1392);
        let message =
            format_uf2_copy_error(Path::new("firmware.uf2"), Path::new("G:/NEW.UF2"), &error);
        assert!(message.contains("error 1392"));
        assert!(message.contains("synthetic FAT volume"));
        assert!(message.contains("Do not run chkdsk"));
        assert!(message.contains("managed picotool fallback"));
        assert!(message.contains("do not press BOOTSEL"));
    }

    #[test]
    fn windows_1006_is_rejected_while_bootsel_marker_remains() {
        let root = tempdir().unwrap();
        let artifact = root.path().join("firmware.uf2");
        let destination = root.path().join("NEW.UF2");
        fs::write(root.path().join("INFO_UF2.TXT"), "Model: Raspberry Pi RP2").unwrap();
        fs::write(&artifact, encode_uf2(&[1, 2, 3])).unwrap();
        let artifact_len = fs::metadata(&artifact).unwrap().len();

        let error = copy_uf2_artifact_with(&artifact, &destination, root.path(), |_, _| {
            Err(Uf2WriteFailure::new(
                io::Error::from_raw_os_error(1006),
                artifact_len,
            ))
        })
        .unwrap_err();

        let message = error.to_string();
        assert!(message.contains("error 1006"));
        assert!(message.contains("did not observe the ROM eject transition"));
        assert!(message.contains("scans or synchronizes removable drives"));
        assert!(message.contains("do not press BOOTSEL"));
        assert!(root.path().join("INFO_UF2.TXT").is_file());
    }

    #[test]
    fn windows_1006_after_complete_write_and_bootsel_eject_is_accepted() {
        let root = tempdir().unwrap();
        let marker = root.path().join("INFO_UF2.TXT");
        let artifact = root.path().join("firmware.uf2");
        let destination = root.path().join("NEW.UF2");
        fs::write(&marker, "Model: Raspberry Pi RP2").unwrap();
        fs::write(&artifact, encode_uf2(&[1, 2, 3])).unwrap();
        let artifact_len = fs::metadata(&artifact).unwrap().len();

        copy_uf2_artifact_with(&artifact, &destination, root.path(), |_, _| {
            fs::remove_file(&marker).unwrap();
            Err(Uf2WriteFailure::new(
                io::Error::from_raw_os_error(1006),
                artifact_len,
            ))
        })
        .unwrap();
    }

    #[test]
    fn msc_transfer_succeeds_first_attempt_without_rediscovery() {
        let mut copies = 0;
        let mut disappearance_checks = 0;
        let mut rediscoveries = 0;
        let transfer = transfer_uf2_with_retries(
            PathBuf::from("vol-a"),
            UF2_TRANSFER_ATTEMPTS,
            |volume| {
                copies += 1;
                Ok(volume.join("NEW.UF2"))
            },
            |_| {
                disappearance_checks += 1;
                true
            },
            || {
                rediscoveries += 1;
                Ok(None)
            },
        )
        .expect("first attempt succeeds");
        assert_eq!(copies, 1);
        assert_eq!(disappearance_checks, 0);
        assert_eq!(rediscoveries, 0);
        assert_eq!(transfer.volume, PathBuf::from("vol-a"));
        assert_eq!(transfer.destination, Path::new("vol-a").join("NEW.UF2"));
    }

    #[test]
    fn msc_transfer_retries_on_a_fresh_enumeration_and_writes_to_the_new_volume() {
        let mut copies = 0;
        let transfer = transfer_uf2_with_retries(
            PathBuf::from("vol-old"),
            UF2_TRANSFER_ATTEMPTS,
            |volume| {
                copies += 1;
                if copies == 1 {
                    assert_eq!(volume, Path::new("vol-old"));
                    Err(FbuildError::DeployFailed("transient hub failure".into()))
                } else {
                    assert_eq!(volume, Path::new("vol-new"));
                    Ok(volume.join("NEW.UF2"))
                }
            },
            |_| true,
            || Ok(Some(PathBuf::from("vol-new"))),
        )
        .expect("second attempt succeeds");
        assert_eq!(copies, 2);
        assert_eq!(transfer.volume, PathBuf::from("vol-new"));
    }

    #[test]
    fn msc_transfer_does_not_retry_while_the_stale_volume_stays_mounted() {
        let mut copies = 0;
        let failure = transfer_uf2_with_retries(
            PathBuf::from("vol-a"),
            UF2_TRANSFER_ATTEMPTS,
            |_| {
                copies += 1;
                Err(FbuildError::DeployFailed("error 121".into()))
            },
            |_| false,
            || Ok(Some(PathBuf::from("vol-a"))),
        )
        .expect_err("a wedged mount must not be retried");
        assert_eq!(copies, 1);
        assert_eq!(failure.attempts, 1);
        assert!(failure.error.to_string().contains("error 121"));
    }

    #[test]
    fn msc_transfer_gives_up_when_no_bootsel_volume_reappears() {
        let failure = transfer_uf2_with_retries(
            PathBuf::from("vol-a"),
            UF2_TRANSFER_ATTEMPTS,
            |_| Err(FbuildError::DeployFailed("device dropped".into())),
            |_| true,
            || Ok(None),
        )
        .expect_err("no re-enumeration means no retry");
        assert_eq!(failure.attempts, 1);
        assert_eq!(failure.volume, PathBuf::from("vol-a"));
    }

    #[test]
    fn msc_transfer_exhausts_the_attempt_budget() {
        let mut copies = 0;
        let failure = transfer_uf2_with_retries(
            PathBuf::from("vol-a"),
            UF2_TRANSFER_ATTEMPTS,
            |_| {
                copies += 1;
                Err(FbuildError::DeployFailed("persistent failure".into()))
            },
            |_| true,
            || Ok(Some(PathBuf::from("vol-a"))),
        )
        .expect_err("every attempt fails");
        assert_eq!(copies, UF2_TRANSFER_ATTEMPTS);
        assert_eq!(failure.attempts, UF2_TRANSFER_ATTEMPTS);
    }

    #[test]
    fn transfer_location_reports_picoboot_when_no_volume_mounted() {
        assert_eq!(
            describe_transfer_location(Some(Path::new("vol-a"))),
            Path::new("vol-a").display().to_string()
        );
        assert_eq!(
            describe_transfer_location(None),
            "PICOBOOT vendor interface"
        );
    }

    #[test]
    fn windows_121_write_timeout_recommends_a_direct_usb_retry() {
        let error = io::Error::from_raw_os_error(121);
        let message =
            format_uf2_copy_error(Path::new("firmware.uf2"), Path::new("G:/NEW.UF2"), &error);
        assert!(message.contains("error 121"));
        assert!(message.contains("direct USB port"));
        assert!(message.contains("avoid USB hubs"));
        assert!(message.contains("do not retry the same timed-out enumeration"));
        assert!(message.contains("do not press BOOTSEL"));
    }
}
