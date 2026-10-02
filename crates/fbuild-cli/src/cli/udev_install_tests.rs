use super::*;

fn desktop() -> HostEnv {
    HostEnv {
        has_display: true,
        stdin_is_tty: true,
        pkexec: Some(PathBuf::from("/usr/bin/pkexec")),
        sudo: Some(PathBuf::from("/usr/bin/sudo")),
        ..HostEnv::default()
    }
}

#[test]
fn root_runs_directly() {
    let env = HostEnv {
        is_root: true,
        ..desktop()
    };
    assert_eq!(choose_elevation(&env), Some(Elevation::Direct));
}

#[test]
fn desktop_prefers_the_polkit_dialog() {
    assert_eq!(
        choose_elevation(&desktop()),
        Some(Elevation::Pkexec(PathBuf::from("/usr/bin/pkexec")))
    );
}

#[test]
fn terminal_without_polkit_uses_sudo() {
    let env = HostEnv {
        pkexec: None,
        has_display: false,
        ..desktop()
    };
    assert_eq!(
        choose_elevation(&env),
        Some(Elevation::Sudo(PathBuf::from("/usr/bin/sudo")))
    );
}

#[test]
fn display_without_tty_or_polkit_uses_sudo_askpass() {
    let env = HostEnv {
        pkexec: None,
        stdin_is_tty: false,
        askpass: Some(PathBuf::from("/usr/bin/ssh-askpass")),
        ..desktop()
    };
    assert_eq!(
        choose_elevation(&env),
        Some(Elevation::SudoAskpass {
            sudo: PathBuf::from("/usr/bin/sudo"),
            askpass: PathBuf::from("/usr/bin/ssh-askpass"),
        })
    );
}

#[test]
fn nothing_to_prompt_with_yields_none() {
    let env = HostEnv {
        sudo: Some(PathBuf::from("/usr/bin/sudo")),
        ..HostEnv::default()
    };
    assert_eq!(choose_elevation(&env), None);
}

#[test]
fn never_prompts_automatically_in_ci_opt_out_or_headless() {
    assert!(may_prompt_automatically(&desktop()));
    assert!(!may_prompt_automatically(&HostEnv {
        ci: true,
        ..desktop()
    }));
    assert!(!may_prompt_automatically(&HostEnv {
        no_elevate: true,
        ..desktop()
    }));
    assert!(!may_prompt_automatically(&HostEnv {
        has_display: false,
        stdin_is_tty: false,
        ..desktop()
    }));
}

#[test]
fn nixos_installs_to_run_because_etc_is_a_store_symlink() {
    assert_eq!(rules_dir(&desktop()), ETC_RULES_DIR);
    assert_eq!(
        rules_dir(&HostEnv {
            nixos: true,
            ..desktop()
        }),
        RUN_RULES_DIR
    );
}

/// pkexec's dialog shows the program it runs, so argv[1] must be the named
/// helper and the destination must come right after it (a truncated dialog
/// still shows it). Every argument is a path fbuild generated.
#[test]
fn pkexec_runs_the_named_helper_with_the_destination_first() {
    let argv = elevated_argv(
        &Elevation::Pkexec(PathBuf::from("/usr/bin/pkexec")),
        Path::new("/tmp/fbuild-udev-x/fbuild-install-usb-rules"),
        Path::new("/run/udev/rules.d/70-fbuild.rules"),
        Path::new("/tmp/fbuild-udev-x/70-fbuild.rules"),
        Path::new("/run/udev/rules.d/99-fbuild.rules"),
        Path::new("/usr/bin/udevadm"),
    );
    assert_eq!(
        argv,
        [
            "/usr/bin/pkexec",
            "/tmp/fbuild-udev-x/fbuild-install-usb-rules",
            "/run/udev/rules.d/70-fbuild.rules",
            "/tmp/fbuild-udev-x/70-fbuild.rules",
            "/run/udev/rules.d/99-fbuild.rules",
            "/usr/bin/udevadm",
        ]
    );
    assert!(
        !argv.iter().any(|a| a == "-c"),
        "no shell one-liner in the dialog"
    );
}

#[test]
fn sudo_askpass_adds_dash_a() {
    let p = Path::new("/p");
    let argv = elevated_argv(
        &Elevation::SudoAskpass {
            sudo: PathBuf::from("/usr/bin/sudo"),
            askpass: PathBuf::from("/usr/bin/ssh-askpass"),
        },
        Path::new("/h"),
        p,
        p,
        p,
        p,
    );
    assert_eq!(&argv[..3], ["/usr/bin/sudo", "-A", "/h"]);
}

#[test]
fn helper_script_installs_to_its_first_argument() {
    assert!(HELPER_SCRIPT.starts_with("#!/bin/sh\n"));
    assert!(HELPER_SCRIPT.contains(r#"install -D -m 0644 "$2" "$1""#));
    assert!(HELPER_SCRIPT.contains("set -e"));
}

/// The user must be told what is installed, why, that it is one-time, and
/// how to do it themselves instead of granting fbuild root.
#[test]
fn explanation_says_what_why_once_and_the_manual_command() {
    let dest = Path::new("/etc/udev/rules.d/70-fbuild.rules");
    let text = explanation(dest, false);
    for needle in [
        "USB device rules",
        "without root",
        "one-time",
        "/etc/udev/rules.d/70-fbuild.rules",
        "To do it yourself instead",
        "sudo tee /etc/udev/rules.d/70-fbuild.rules",
        "udevadm control --reload-rules",
    ] {
        assert!(text.contains(needle), "missing {needle:?} in:\n{text}");
    }
    assert!(explanation(dest, true).contains("until reboot"));
}

#[test]
fn rules_current_compares_the_installed_file() {
    let dir = tempfile::TempDir::new().unwrap();
    assert!(!rules_current(dir.path(), "R\n"));
    std::fs::write(dir.path().join(UDEV_RULES_FILENAME), "R\n").unwrap();
    assert!(rules_current(dir.path(), "R\n"));
    assert!(!rules_current(dir.path(), "R2\n"));
}

#[test]
fn nixos_snippet_wraps_only_the_rules() {
    let s = nixos_snippet("# header\nSUBSYSTEM==\"usb\", X\n");
    assert_eq!(
        s,
        "services.udev.extraRules = ''\n    SUBSYSTEM==\"usb\", X\n'';\n"
    );
}
