//! `fbuild show <target>` plus the daemon log tail used by both `show
//! daemon` and `daemon monitor`.

use crate::output;

pub fn run_show(target: &str, follow: bool, lines: usize) -> fbuild_core::Result<()> {
    match target {
        "daemon" => show_daemon_logs(follow, lines),
        other => {
            output::error(format!(
                "unknown show target: '{}' (available: daemon)",
                other
            ));
            std::process::exit(1);
        }
    }
}

pub fn show_daemon_logs(follow: bool, initial_lines: usize) -> fbuild_core::Result<()> {
    let log_path = fbuild_paths::get_daemon_log_file();
    if !log_path.exists() {
        output::error(format!(
            "daemon log file not found: {}",
            log_path.display()
        ));
        output::error("the daemon may not have been started yet");
        return Ok(());
    }

    let content = std::fs::read_to_string(&log_path)
        .map_err(|e| fbuild_core::FbuildError::Other(format!("failed to read log file: {}", e)))?;

    // Show last N lines
    let all_lines: Vec<&str> = content.lines().collect();
    let start = all_lines.len().saturating_sub(initial_lines);
    for line in &all_lines[start..] {
        output::result(*line);
    }

    if !follow {
        return Ok(());
    }

    // Follow mode: poll for new content
    output::progress(format!(
        "--- following {} (Ctrl+C to stop) ---",
        log_path.display()
    ));
    let mut pos = content.len() as u64;
    loop {
        std::thread::sleep(std::time::Duration::from_millis(100));
        let current_len = std::fs::metadata(&log_path).map(|m| m.len()).unwrap_or(pos);

        if current_len > pos {
            use std::io::{Read, Seek};
            if let Ok(mut file) = std::fs::File::open(&log_path) {
                let _ = file.seek(std::io::SeekFrom::Start(pos));
                let mut buf = String::new();
                if file.read_to_string(&mut buf).is_ok() && !buf.is_empty() {
                    // Buffered tail content may end with '\n'; strip it so
                    // result()'s appended newline doesn't double up.
                    output::result(buf.trim_end_matches('\n'));
                }
                pos = current_len;
            }
        } else if current_len < pos {
            // Log file was truncated/rotated — re-read from start
            pos = 0;
        }
    }
}
