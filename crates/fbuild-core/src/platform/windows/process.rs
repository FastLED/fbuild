use std::os::windows::io::AsHandle;
use std::sync::OnceLock;

use crate::path::NormalizedPath;
use crate::platform::process::{DetachedEnvironment, Termination};

type Handle = *mut std::ffi::c_void;

#[derive(Clone, Copy)]
struct JobHandle(Handle);

// SAFETY: a Windows kernel HANDLE is an opaque process-wide value; this wrapper
// never dereferences it, and the intentionally leaked job outlives all threads.
unsafe impl Send for JobHandle {}
// SAFETY: all operations on the shared job handle are kernel-synchronized.
unsafe impl Sync for JobHandle {}

static TOKIO_JOB: OnceLock<JobHandle> = OnceLock::new();

pub(crate) fn configure_tokio_owner_death(
    _command: &mut tokio::process::Command,
) -> std::io::Result<()> {
    Ok(())
}

pub(crate) fn after_tokio_spawn(child: &tokio::process::Child) -> std::io::Result<()> {
    let Some(raw) = child.raw_handle() else {
        return Ok(());
    };
    let job = ensure_job()?;
    // SAFETY: `raw` is borrowed from the live Tokio child and `job` is the
    // process-lifetime handle returned by `ensure_job`.
    let ok = unsafe { AssignProcessToJobObject(job, raw) };
    if ok == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

pub(crate) fn spawn_detached(
    command: &mut std::process::Command,
    stderr: Option<&std::fs::File>,
    environment: DetachedEnvironment,
) -> std::io::Result<u32> {
    let stderr = match stderr {
        Some(file) => running_process::DaemonStdioSource::Handle(file.as_handle()),
        None => running_process::DaemonStdioSource::Null,
    };
    let child = running_process::spawn_daemon_with_stdio_and_env_policy(
        command,
        running_process::DaemonStdio {
            stdout: running_process::DaemonStdioSource::Null,
            stderr,
        },
        match environment {
            DetachedEnvironment::Inherit => running_process::EnvironmentPolicy::Inherit,
            DetachedEnvironment::Clear => running_process::EnvironmentPolicy::Clear,
        },
    )?;
    Ok(child.id())
}

pub(crate) fn pid_is_alive(pid: u32) -> bool {
    // SAFETY: OpenProcess accepts a numeric PID and returns an owned handle.
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if handle.is_null() {
        return false;
    }
    let mut code = 0;
    // SAFETY: `handle` is live and `code` is a writable local value.
    let ok = unsafe { GetExitCodeProcess(handle, &mut code) };
    // SAFETY: close the owned handle exactly once after its final use.
    unsafe { CloseHandle(handle) };
    ok != 0 && code == STILL_ACTIVE
}

pub(crate) fn pid_executable_path(pid: u32) -> Option<NormalizedPath> {
    // SAFETY: OpenProcess accepts a numeric PID and returns an owned handle.
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if handle.is_null() {
        return None;
    }
    let mut buffer = vec![0_u16; 1024];
    let mut size = buffer.len() as u32;
    // SAFETY: the buffer has `size` writable u16 elements and both out-pointers
    // remain valid for the duration of the call.
    let ok = unsafe { QueryFullProcessImageNameW(handle, 0, buffer.as_mut_ptr(), &mut size) };
    // SAFETY: close the owned handle exactly once after its final use.
    unsafe { CloseHandle(handle) };
    if ok == 0 {
        return None;
    }
    let image = String::from_utf16_lossy(&buffer[..size as usize]);
    (!image.is_empty()).then(|| NormalizedPath::from(image))
}

pub(crate) fn exe_stem_matches(actual: &str, expected: &str) -> bool {
    actual.eq_ignore_ascii_case(expected)
}

pub(crate) fn terminate_pid(pid: u32, _termination: Termination) -> std::io::Result<()> {
    // SAFETY: OpenProcess accepts a numeric PID and returns an owned handle.
    let handle = unsafe { OpenProcess(PROCESS_TERMINATE, 0, pid) };
    if handle.is_null() {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: `handle` is live and was opened with PROCESS_TERMINATE access.
    let ok = unsafe { TerminateProcess(handle, 1) };
    // SAFETY: close the owned handle exactly once after its final use.
    unsafe { CloseHandle(handle) };
    if ok == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

pub(crate) fn exit_code(status: std::process::ExitStatus) -> i32 {
    status.code().unwrap_or(-1)
}

pub(crate) fn command_environment(
    program: &str,
    overlay: Option<&[(&str, &str)]>,
) -> Option<Vec<(String, String)>> {
    let mut environment: std::collections::BTreeMap<String, String> =
        std::env::vars().collect();
    if let Some(executable_dir) = std::path::Path::new(program).parent() {
        let executable_dir = executable_dir.to_string_lossy();
        if !executable_dir.is_empty() {
            let current_path = environment
                .get("PATH")
                .or_else(|| environment.get("Path"))
                .cloned()
                .unwrap_or_default();
            environment.insert("PATH".to_string(), format!("{executable_dir};{current_path}"));
        }
    }
    if environment.contains_key("MSYSTEM") || environment.contains_key("MSYS") {
        strip_msys_environment(&mut environment);
    }
    if let Some(overlay) = overlay {
        for (key, value) in overlay {
            environment.retain(|existing, _| !existing.eq_ignore_ascii_case(key));
            environment.insert((*key).to_string(), (*value).to_string());
        }
    }
    Some(environment.into_iter().collect())
}

fn strip_msys_environment(environment: &mut std::collections::BTreeMap<String, String>) {
    const PREFIXES: &[&str] = &["MSYS", "MINGW", "CHERE", "ORIGINAL_PATH"];
    const EXACT: &[&str] = &[
        "SHELL", "SHLVL", "TERM", "TERM_PROGRAM", "TERM_PROGRAM_VERSION", "TMPDIR", "TMP",
        "TEMP", "_", "!", "POSIXLY_CORRECT", "EXECIGNORE", "HOSTTYPE", "MACHTYPE", "OSTYPE",
        "CONFIG_SITE",
    ];
    environment.retain(|key, _| {
        !PREFIXES.iter().any(|prefix| key.starts_with(prefix)) && !EXACT.contains(&key.as_str())
    });
    if let Some(path) = environment.get("PATH").cloned() {
        let filtered = path
            .split(';')
            .filter(|entry| {
                let lower = entry.to_lowercase();
                !entry.starts_with('/')
                    && !lower.contains("\\msys")
                    && !lower.contains("/msys")
                    && !lower.contains("/usr/")
            })
            .collect::<Vec<_>>()
            .join(";");
        environment.insert("PATH".to_string(), filtered);
    }
}

#[cfg(test)]
pub(crate) fn create_path_probe(directory: &std::path::Path) -> std::io::Result<super::super::process::PathProbe> {
    let system_root = std::env::var_os("SystemRoot").ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::NotFound, "SystemRoot is not set")
    })?;
    let source = std::path::Path::new(&system_root)
    .join("System32")
    .join("cmd.exe");
    let path = directory.join("fbuild_1219_probe.exe");
    std::fs::copy(source, &path)?;
    Ok(super::super::process::PathProbe {
        path: path.into(),
        bare_args: vec![
            "fbuild_1219_probe".to_string(),
            "/C".to_string(),
            "echo overlay-marker".to_string(),
        ],
    })
}

fn ensure_job() -> std::io::Result<Handle> {
    if let Some(job) = TOKIO_JOB.get() {
        return Ok(job.0);
    }
    // SAFETY: null security/name pointers request documented defaults and the
    // returned handle is retained for the process lifetime.
    let job = unsafe { CreateJobObjectW(std::ptr::null_mut(), std::ptr::null()) };
    if job.is_null() {
        return Err(std::io::Error::last_os_error());
    }
    let mut info = JobObjectExtendedLimitInformation::default();
    info.basic_limit_information.limit_flags =
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE | JOB_OBJECT_LIMIT_BREAKAWAY_OK;
    // SAFETY: `info` has the ABI layout declared below and remains writable for
    // the exact byte length supplied; `job` is a live owned job handle.
    let ok = unsafe {
        SetInformationJobObject(
            job,
            JOB_OBJECT_EXTENDED_LIMIT_INFORMATION,
            (&mut info as *mut JobObjectExtendedLimitInformation).cast(),
            std::mem::size_of::<JobObjectExtendedLimitInformation>() as u32,
        )
    };
    if ok == 0 {
        let error = std::io::Error::last_os_error();
        // SAFETY: configuration failed, so this thread still solely owns the
        // valid job handle and closes it exactly once before returning.
        unsafe { CloseHandle(job) };
        return Err(error);
    }
    if let Err(loser) = TOKIO_JOB.set(JobHandle(job)) {
        // SAFETY: another thread installed the process-lifetime job first;
        // `loser` is the valid, unshared handle created by this thread.
        unsafe { CloseHandle(loser.0) };
    }
    Ok(TOKIO_JOB.get().expect("job was initialized").0)
}

const PROCESS_TERMINATE: u32 = 0x0001;
const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
const STILL_ACTIVE: u32 = 259;
const JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE: u32 = 0x2000;
const JOB_OBJECT_LIMIT_BREAKAWAY_OK: u32 = 0x0800;
const JOB_OBJECT_EXTENDED_LIMIT_INFORMATION: i32 = 9;

#[repr(C)]
#[derive(Default)]
struct IoCounters {
    read_operation_count: u64,
    write_operation_count: u64,
    other_operation_count: u64,
    read_transfer_count: u64,
    write_transfer_count: u64,
    other_transfer_count: u64,
}

#[repr(C)]
#[derive(Default)]
struct JobObjectBasicLimitInformation {
    per_process_user_time_limit: i64,
    per_job_user_time_limit: i64,
    limit_flags: u32,
    minimum_working_set_size: usize,
    maximum_working_set_size: usize,
    active_process_limit: u32,
    affinity: usize,
    priority_class: u32,
    scheduling_class: u32,
}

#[repr(C)]
#[derive(Default)]
struct JobObjectExtendedLimitInformation {
    basic_limit_information: JobObjectBasicLimitInformation,
    io_info: IoCounters,
    process_memory_limit: usize,
    job_memory_limit: usize,
    peak_process_memory_used: usize,
    peak_job_memory_used: usize,
}

#[link(name = "kernel32")]
extern "system" {
    fn CreateJobObjectW(security_attrs: Handle, name: *const u16) -> Handle;
    fn SetInformationJobObject(
        job: Handle,
        info_class: i32,
        info: Handle,
        info_len: u32,
    ) -> i32;
    fn AssignProcessToJobObject(job: Handle, process: Handle) -> i32;
    fn OpenProcess(desired_access: u32, inherit: i32, pid: u32) -> Handle;
    fn CloseHandle(handle: Handle) -> i32;
    fn GetExitCodeProcess(handle: Handle, exit_code: *mut u32) -> i32;
    fn QueryFullProcessImageNameW(
        handle: Handle,
        flags: u32,
        buffer: *mut u16,
        size: *mut u32,
    ) -> i32;
    fn TerminateProcess(handle: Handle, exit_code: u32) -> i32;
}
