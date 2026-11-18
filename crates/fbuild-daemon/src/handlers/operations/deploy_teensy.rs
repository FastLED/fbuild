//! `teensy_loader_cli` discovery for the Teensy deploy arm.

use fbuild_core::path::NormalizedPath;
use fbuild_core::platform::executable::{find_tool_in, find_tool_on_paths};
use std::ffi::OsStr;
use std::path::Path;

const STEM: &str = "teensy_loader_cli";

/// Resolve a usable `teensy_loader_cli` binary for the Teensy deploy arm.
///
/// Search order:
///   1. the requesting CLI's forwarded `PATH` (`caller_path`,
///      FastLED/fbuild#1234) — the daemon's own environment can be stale or
///      minimal, so a tool the user installed in their shell must win;
///   2. the daemon's `$PATH`;
///   3. `~/.platformio/packages/tool-teensy/` — the well-known path
///      PlatformIO installs it at on every PIO-using machine.
///
/// Each directory accepts `teensy_loader_cli` (Unix) / `teensy_loader_cli.exe`
/// (Windows), or a `.com`/`.exe` APE on any host.
///
/// Returns `None` if nothing is found; the TeensyDeployer's default will then
/// try a bare `teensy_loader_cli` invocation, which will surface
/// `command not found` to the user — clearer than a silent abort here.
pub(super) fn find_teensy_loader_cli(caller_path: Option<&str>) -> Option<NormalizedPath> {
    let home = if fbuild_core::platform::host::is_windows() {
        std::env::var_os("USERPROFILE")
    } else {
        std::env::var_os("HOME")
    };
    find_teensy_loader_cli_in(
        caller_path.map(OsStr::new),
        std::env::var_os("PATH").as_deref(),
        home.as_deref().map(Path::new),
    )
}

/// [`find_teensy_loader_cli`] with every environment input passed explicitly.
fn find_teensy_loader_cli_in(
    caller_path: Option<&OsStr>,
    daemon_path: Option<&OsStr>,
    home: Option<&Path>,
) -> Option<NormalizedPath> {
    for path_env in [caller_path, daemon_path].into_iter().flatten() {
        if let Some(found) = find_tool_on_paths(std::env::split_paths(path_env), STEM) {
            return Some(found);
        }
    }

    // PlatformIO drops the binary here on every platform. Reusing it means a
    // user who already has PIO working doesn't need to install anything else
    // to deploy via fbuild.
    let pio_tool_dir = home?
        .join(".platformio")
        .join("packages")
        .join("tool-teensy");
    find_tool_in(&pio_tool_dir, STEM)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    async fn write_ape(path: &Path) {
        fbuild_core::fs::write(path, b"MZqFpD='\n#!/bin/sh\n")
            .await
            .unwrap();
    }

    fn join(dirs: &[&Path]) -> OsString {
        std::env::join_paths(dirs).unwrap()
    }

    /// CodeRabbit on FastLED/fbuild#1633: a `teensy_loader_cli.com` APE that
    /// exists only on the requesting CLI's PATH must be found even though the
    /// daemon's own PATH does not contain it.
    #[tokio::test]
    async fn caller_path_ape_is_found_when_daemon_path_lacks_it() {
        let caller = tempfile::TempDir::new().unwrap();
        let daemon = tempfile::TempDir::new().unwrap();
        let tool = caller.path().join("teensy_loader_cli.com");
        write_ape(&tool).await;

        let found = find_teensy_loader_cli_in(
            Some(&join(&[caller.path()])),
            Some(&join(&[daemon.path()])),
            None,
        );
        assert_eq!(found, Some(NormalizedPath::from(tool)));
    }

    #[tokio::test]
    async fn caller_path_wins_over_daemon_path() {
        let caller = tempfile::TempDir::new().unwrap();
        let daemon = tempfile::TempDir::new().unwrap();
        let caller_tool = caller.path().join("teensy_loader_cli.com");
        write_ape(&caller_tool).await;
        write_ape(&daemon.path().join("teensy_loader_cli.com")).await;

        let found = find_teensy_loader_cli_in(
            Some(&join(&[caller.path()])),
            Some(&join(&[daemon.path()])),
            None,
        );
        assert_eq!(found, Some(NormalizedPath::from(caller_tool)));
    }

    #[tokio::test]
    async fn falls_back_to_daemon_path_then_platformio() {
        let caller = tempfile::TempDir::new().unwrap();
        let daemon = tempfile::TempDir::new().unwrap();
        let home = tempfile::TempDir::new().unwrap();
        let daemon_tool = daemon.path().join("teensy_loader_cli.com");
        write_ape(&daemon_tool).await;

        let found = find_teensy_loader_cli_in(
            Some(&join(&[caller.path()])),
            Some(&join(&[daemon.path()])),
            Some(home.path()),
        );
        assert_eq!(found, Some(NormalizedPath::from(daemon_tool)));

        let pio_dir = home.path().join(".platformio/packages/tool-teensy");
        fbuild_core::fs::create_dir_all(&pio_dir).await.unwrap();
        let pio_tool = pio_dir.join("teensy_loader_cli.com");
        write_ape(&pio_tool).await;
        let found =
            find_teensy_loader_cli_in(Some(&join(&[caller.path()])), None, Some(home.path()));
        assert_eq!(found, Some(NormalizedPath::from(pio_tool)));
    }
}
