use super::*;

struct Fixture {
    path: NormalizedPath,
    temp: Option<tempfile::TempDir>,
}

impl Fixture {
    fn path(&self) -> &NormalizedPath {
        &self.path
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if self.temp.is_none() {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }
}

fn tempdir() -> Fixture {
    if fbuild_core::platform::host::is_windows() {
        let base = NormalizedPath::new(fbuild_core::platform::fs::short_cache_base().unwrap());
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let nonce = format!(
            "{}|{}|{}",
            std::process::id(),
            std::time::UNIX_EPOCH.elapsed().unwrap().as_nanos(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
        );
        let path = base.join(format!(
            "p{}",
            &blake3::hash(nonce.as_bytes()).to_hex().as_str()[..12]
        ));
        // Native creation explicitly sets TokenUser ownership. A default
        // TempDir's TokenOwner may be the Administrators group when elevated.
        fs::ensure_private_directory(&path).unwrap();
        Fixture { path, temp: None }
    } else {
        let temp = tempfile::Builder::new().prefix("p").tempdir().unwrap();
        Fixture {
            path: NormalizedPath::new(temp.path()),
            temp: Some(temp),
        }
    }
}

fn inputs(temp: &Fixture, observation: Observation) -> Inputs {
    let home = temp.path().join("home");
    let short_base = temp.path().clone();
    Inputs {
        fbuild_root: home.join(crate::FBUILD_DIR_NAME).join("prod"),
        home,
        short_base,
        cache_override: None,
        build_override: None,
        dev_mode: false,
        observation,
    }
}

#[test]
fn both_path_override_knobs_accept_current_directory_spellings() {
    let expected = NormalizedPath::new(std::env::current_dir().unwrap());
    for knob in ["FBUILD_CACHE_DIR", "FBUILD_BUILD_DIR"] {
        for value in [".", "child/.."] {
            let selected = override_from_value(Some(std::ffi::OsString::from(value))).unwrap();
            assert_eq!(selected, Some(expected.clone()), "{knob}={value}");
        }
    }
}

#[test]
fn path_override_parser_rejects_actual_empty_value_and_preserves_absence() {
    assert!(override_from_value(Some(std::ffi::OsString::new())).is_err());
    assert_eq!(override_from_value(None).unwrap(), None);
}

#[test]
fn disabled_policy_creates_short_writable_cache_and_scratch() {
    let temp = tempdir();
    let resolution = resolve(inputs(&temp, Observation::Disabled)).unwrap();
    assert!(resolution.cache_root.relative_to(temp.path()).is_some());
    assert!(resolution.cache_root.is_dir());
    assert!(resolution.scratch_root.is_dir());
    assert_ne!(resolution.cache_root, resolution.requested_cache_root);
    assert_eq!(resolution.cache_dir_source, "windows-short-path-fallback");
    let advisory = resolution.advisory().unwrap();
    assert!(advisory.contains("disabled"));
    assert!(advisory.contains("fbuild --fix-win"));
    assert!(advisory.contains(&resolution.cache_root.display().to_string()));
}

#[test]
fn forwarding_a_selected_fallback_build_root_preserves_its_identity() {
    let temp = tempdir();
    let mut input = inputs(&temp, Observation::Disabled);
    let padding = 50 - fs::legacy_path_units(temp.path()) - 1;
    input.cache_override = Some(temp.path().join("c".repeat(padding)));
    let resolution = resolve(input).unwrap();
    let project = temp.path().join("project");
    let requested = project.join("deep-component".repeat(12)).join("build");
    let selected = resolution.resolve_build_root(&project, &requested);
    assert_eq!(resolution.resolve_build_root(&project, &selected), selected);
}

#[test]
fn short_explicit_cache_is_preserved_and_still_warns() {
    let temp = tempdir();
    let mut input = inputs(&temp, Observation::Disabled);
    let selected = temp.path().join("explicit");
    input.cache_override = Some(selected.clone());
    let resolution = resolve(input).unwrap();
    assert_eq!(resolution.cache_root, selected);
    assert_eq!(resolution.cache_dir_source, "FBUILD_CACHE_DIR");
    assert!(resolution.advisory().is_some());
    assert!(resolution.scratch_root.relative_to(&selected).is_some());
}

#[test]
fn deep_explicit_cache_uses_fallback_and_reports_both_paths() {
    let temp = tempdir();
    let mut input = inputs(&temp, Observation::Disabled);
    let requested = temp.path().join("long".repeat(30));
    input.cache_override = Some(requested.clone());
    let resolution = resolve(input).unwrap();
    assert_ne!(resolution.cache_root, requested);
    assert_eq!(resolution.requested_cache_root, requested);
    let advisory = resolution.advisory().unwrap();
    assert!(advisory.contains(&requested.display().to_string()));
    assert!(advisory.contains(&resolution.cache_root.display().to_string()));
}

#[test]
fn enabled_policy_retains_automatic_cache_from_durable_marker() {
    let temp = tempdir();
    let first = resolve(inputs(&temp, Observation::Disabled)).unwrap();
    std::fs::write(first.cache_root.join("package"), b"existing package").unwrap();
    let next = resolve(inputs(&temp, Observation::Enabled)).unwrap();
    assert_eq!(first.cache_root, next.cache_root);
    assert_eq!(
        next.cache_dir_source,
        "retained-windows-short-path-fallback"
    );
    assert!(next.advisory().is_none());
    assert_eq!(
        std::fs::read(next.cache_root.join("package")).unwrap(),
        b"existing package"
    );
}

#[test]
fn explicit_cache_change_wins_over_retained_selection() {
    let temp = tempdir();
    resolve(inputs(&temp, Observation::Disabled)).unwrap();
    let mut input = inputs(&temp, Observation::Enabled);
    let new_cache = temp.path().join("new-cache");
    input.cache_override = Some(new_cache.clone());
    assert_eq!(resolve(input).unwrap().cache_root, new_cache);
}

#[test]
fn short_explicit_cache_works_when_automatic_base_is_unavailable() {
    let temp = tempdir();
    let mut input = inputs(&temp, Observation::Disabled);
    input.short_base = temp.path().join("unavailable-base");
    std::fs::write(&input.short_base, b"not a directory").unwrap();
    let selected = temp.path().join("explicit");
    input.cache_override = Some(selected.clone());
    let resolution = resolve(input).unwrap();
    assert_eq!(resolution.cache_root, selected);
    assert!(resolution.scratch_root.relative_to(&selected).is_some());
    assert!(
        resolution
            .short_build_root
            .unwrap()
            .relative_to(&selected)
            .is_some()
    );
}

#[test]
fn enabled_policy_does_not_query_an_unused_short_base() {
    let temp = tempdir();
    let input = inputs(&temp, Observation::Enabled);
    let queried = std::cell::Cell::new(false);
    let resolution = resolve_with_short_base(input, || {
        queried.set(true);
        Err(std::io::Error::other("SystemDrive unavailable"))
    })
    .unwrap();
    assert!(!queried.get());
    assert_eq!(resolution.cache_dir_source, "default");
}

#[test]
fn enabled_explicit_cache_has_no_legacy_budget_or_drive_dependency() {
    let temp = tempdir();
    let mut input = inputs(&temp, Observation::Enabled);
    let selected = temp.path().join("long".repeat(30));
    input.cache_override = Some(selected.clone());
    let resolution = resolve_with_short_base(input, || {
        panic!("an enabled explicit cache must not query the automatic drive")
    })
    .unwrap();
    assert_eq!(resolution.cache_root, selected);
    assert_eq!(resolution.cache_dir_source, "FBUILD_CACHE_DIR");
}

#[test]
fn retained_marker_cannot_select_a_different_users_cache() {
    let temp = tempdir();
    let input = inputs(&temp, Observation::Disabled);
    let marker_path = input.fbuild_root.join(MARKER_FILE);
    let first = resolve(input).unwrap();
    let marker = SelectionMarker {
        schema: MARKER_SCHEMA,
        cache_root: temp.path().join("unrelated-cache"),
        scratch_root: first.scratch_root,
    };
    std::fs::write(&marker_path, serde_json::to_vec(&marker).unwrap()).unwrap();
    assert!(resolve(inputs(&temp, Observation::Enabled)).is_err());
}

#[test]
fn malformed_retention_marker_is_reported() {
    let temp = tempdir();
    let input = inputs(&temp, Observation::Disabled);
    let marker_path = input.fbuild_root.join(MARKER_FILE);
    resolve(input).unwrap();
    std::fs::write(marker_path, b"not JSON").unwrap();
    assert!(resolve(inputs(&temp, Observation::Enabled)).is_err());
}

#[test]
fn explicit_cache_is_normalized_before_shortness_is_checked() {
    let temp = tempdir();
    let mut input = inputs(&temp, Observation::Disabled);
    input.cache_override = Some(temp.path().join("long".repeat(30)).join("..").join("cache"));
    let resolution = resolve(input).unwrap();
    assert_eq!(resolution.cache_root, temp.path().join("cache"));
    assert_eq!(resolution.cache_dir_source, "FBUILD_CACHE_DIR");
}

#[test]
fn repeated_fallback_initialization_preserves_valid_marker() {
    let temp = tempdir();
    let first = resolve(inputs(&temp, Observation::Disabled)).unwrap();
    let marker_path = first.fbuild_root.join(MARKER_FILE);
    let mut saved: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&marker_path).unwrap()).unwrap();
    saved["preserve_extra_field"] = serde_json::json!(true);
    let bytes = serde_json::to_vec(&saved).unwrap();
    std::fs::write(&marker_path, &bytes).unwrap();
    let second = resolve(inputs(&temp, Observation::Disabled)).unwrap();
    assert_eq!(first.cache_root, second.cache_root);
    assert_eq!(std::fs::read(&marker_path).unwrap(), bytes);
    let enabled = resolve(inputs(&temp, Observation::Enabled)).unwrap();
    assert_eq!(first.cache_root, enabled.cache_root);
}

#[test]
fn concurrent_first_fallback_initialization_retains_one_valid_selection() {
    let temp = tempdir();
    let barrier = std::sync::Barrier::new(4);
    let roots = std::thread::scope(|scope| {
        let contenders: Vec<_> = (0..4)
            .map(|_| {
                scope.spawn(|| {
                    barrier.wait();
                    resolve(inputs(&temp, Observation::Disabled))
                        .unwrap()
                        .cache_root
                })
            })
            .collect();
        contenders
            .into_iter()
            .map(|contender| contender.join().unwrap())
            .collect::<Vec<_>>()
    });
    assert!(roots.iter().all(|root| root == &roots[0]));
    let retained = resolve(inputs(&temp, Observation::Enabled)).unwrap();
    assert_eq!(retained.cache_root, roots[0]);
}

#[test]
fn concurrent_short_explicit_cache_initialization_shares_real_sidecars() {
    let temp = tempdir();
    for index in 0..8 {
        let selected = temp.path().join(format!("e{index}"));
        let barrier = std::sync::Barrier::new(4);
        std::thread::scope(|scope| {
            let contenders: Vec<_> = (0..4)
                .map(|_| {
                    scope.spawn(|| {
                        let mut input = inputs(&temp, Observation::Disabled);
                        input.cache_override = Some(selected.clone());
                        barrier.wait();
                        resolve(input).unwrap()
                    })
                })
                .collect();
            for contender in contenders {
                let resolution = contender.join().unwrap();
                assert_eq!(resolution.cache_root, selected);
                assert!(resolution.scratch_root.is_dir());
                assert!(resolution.short_build_root.unwrap().is_dir());
            }
        });
    }
}

#[test]
fn unknown_policy_uses_fallback_with_query_error_in_warning() {
    let temp = tempdir();
    let resolution = resolve(inputs(&temp, Observation::Unknown("access denied".into()))).unwrap();
    assert!(resolution.short_build_root.is_some());
    let advisory = resolution.advisory().unwrap();
    assert!(advisory.contains("could not be checked"));
    assert!(advisory.contains("access denied"));
}

#[test]
fn dev_and_prod_fallbacks_do_not_share_artifacts() {
    let temp = tempdir();
    let prod = resolve(inputs(&temp, Observation::Disabled)).unwrap();
    let mut input = inputs(&temp, Observation::Disabled);
    input.dev_mode = true;
    input.fbuild_root = input.home.join(crate::FBUILD_DIR_NAME).join("dev");
    let dev = resolve(input).unwrap();
    assert_ne!(dev.cache_root, prod.cache_root);
    assert_ne!(dev.scratch_root, prod.scratch_root);
    assert_ne!(dev.short_build_root, prod.short_build_root);
}

#[test]
fn deep_build_outputs_are_keyed_by_normalized_project() {
    let temp = tempdir();
    let resolution = resolve(inputs(&temp, Observation::Disabled)).unwrap();
    let first = temp.path().join("project-one");
    let second = temp.path().join("project-two");
    std::fs::create_dir_all(&first).unwrap();
    std::fs::create_dir_all(&second).unwrap();
    let deep = first.join("deep".repeat(35));
    let first_root = resolution.resolve_build_root(&first, &deep);
    let second_root = resolution.resolve_build_root(&second, &deep);
    assert!(
        first_root
            .relative_to(resolution.short_build_root.as_ref().unwrap())
            .is_some()
    );
    assert_ne!(first_root, second_root);
    assert_eq!(
        first_root,
        resolution.resolve_build_root(&first.join("."), &deep)
    );
    let short = temp.path().join("build");
    assert_eq!(resolution.resolve_build_root(&first, &short), short);
}

#[test]
fn short_build_override_returns_normalized_path() {
    let temp = tempdir();
    let resolution = resolve(inputs(&temp, Observation::Disabled)).unwrap();
    let project = temp.path().join("project");
    std::fs::create_dir_all(&project).unwrap();
    let requested = temp.path().join("long".repeat(30)).join("..").join("build");
    assert_eq!(
        resolution.resolve_build_root(&project, &requested),
        temp.path().join("build"),
    );
}

#[test]
fn distinct_deep_build_overrides_for_same_project_do_not_collide() {
    let temp = tempdir();
    let resolution = resolve(inputs(&temp, Observation::Disabled)).unwrap();
    let project = temp.path().join("project");
    std::fs::create_dir_all(&project).unwrap();
    let first = project.join("first".repeat(30));
    let second = project.join("second".repeat(30));
    let first_resolved = resolution.resolve_build_root(&project, &first);
    let second_resolved = resolution.resolve_build_root(&project, &second);
    assert_ne!(first_resolved, second_resolved);
    assert_eq!(
        first_resolved,
        resolution.resolve_build_root(&project.join("."), &first.join(".")),
    );
    resolution.validate_legacy_path(&first_resolved).unwrap();
    resolution.validate_legacy_path(&second_resolved).unwrap();
}

#[test]
fn prepares_private_automatic_build_directory_before_writing_outputs() {
    let temp = tempdir();
    let resolution = resolve(inputs(&temp, Observation::Disabled)).unwrap();
    let project = temp.path().join("project");
    let deep = project.join("deep".repeat(30));
    let build = resolution
        .resolve_build_root(&project, &deep)
        .join("env")
        .join("release");
    resolution.prepare_build_directory(&build).unwrap();
    assert!(build.is_dir());
    assert!(!fs::is_link_or_reparse(&build).unwrap());
}

#[test]
fn private_build_preparation_rejects_linked_project_ancestor() {
    let temp = tempdir();
    let resolution = resolve(inputs(&temp, Observation::Disabled)).unwrap();
    let project = temp.path().join("project");
    let deep = project.join("deep".repeat(30));
    let build = resolution.resolve_build_root(&project, &deep);
    let base = resolution.short_build_root.as_ref().unwrap();
    let project_root = base.join(
        build
            .relative_to(base)
            .unwrap()
            .components()
            .next()
            .unwrap()
            .as_os_str(),
    );
    let outside = temp.path().join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    if !link_fixture(&outside, &project_root) {
        return;
    }
    assert!(
        resolution
            .prepare_build_directory(&build.join("env").join("release"))
            .is_err()
    );
    assert_eq!(std::fs::read_dir(outside).unwrap().count(), 0);
}

#[test]
fn build_preparation_leaves_intentional_cache_and_project_paths_untouched() {
    let temp = tempdir();
    let mut input = inputs(&temp, Observation::Disabled);
    input.cache_override = Some(temp.path().join("explicit"));
    let explicit = resolve(input).unwrap();
    let project = temp.path().join("project");
    let deep = project.join("deep".repeat(30));
    let manual_build = explicit.resolve_build_root(&project, &deep);
    explicit.prepare_build_directory(&manual_build).unwrap();
    assert!(!manual_build.exists());
    let automatic = resolve(inputs(&temp, Observation::Disabled)).unwrap();
    let local = temp.path().join("build");
    automatic.prepare_build_directory(&local).unwrap();
    assert!(!local.exists());
}

fn link_fixture(original: &Path, link: &Path) -> bool {
    match fs::symlink_dir(original, link) {
        Ok(()) => true,
        Err(error)
            if fbuild_core::platform::host::is_windows()
                && error.kind() == std::io::ErrorKind::PermissionDenied =>
        {
            false
        }
        Err(error) => panic!("cannot create directory link fixture: {error}"),
    }
}

#[test]
fn fallback_rejects_linked_private_user_root_without_writing_through_it() {
    let temp = tempdir();
    let input = inputs(&temp, Observation::Disabled);
    let private_root = input
        .short_base
        .join(format!("fb-{}", path_hash(&input.home)));
    let outside = temp.path().join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    if !link_fixture(&outside, &private_root) {
        return;
    }
    assert!(resolve(input).is_err());
    assert_eq!(std::fs::read_dir(outside).unwrap().count(), 0);
}

#[test]
fn fallback_rejects_linked_mode_without_writing_through_it() {
    let temp = tempdir();
    let input = inputs(&temp, Observation::Disabled);
    let private_root = input
        .short_base
        .join(format!("fb-{}", path_hash(&input.home)));
    fs::ensure_private_directory(&private_root).unwrap();
    let outside = temp.path().join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    if !link_fixture(&outside, &private_root.join("prod")) {
        return;
    }
    assert!(resolve(input).is_err());
    assert_eq!(std::fs::read_dir(outside).unwrap().count(), 0);
}

#[test]
fn fallback_rejects_linked_cache_without_writing_through_it() {
    let temp = tempdir();
    let input = inputs(&temp, Observation::Disabled);
    let mode_root = input
        .short_base
        .join(format!("fb-{}", path_hash(&input.home)))
        .join("prod");
    fs::ensure_private_directory(&mode_root).unwrap();
    let outside = temp.path().join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    if !link_fixture(&outside, &mode_root.join("cache")) {
        return;
    }
    assert!(resolve(input).is_err());
    assert_eq!(std::fs::read_dir(outside).unwrap().count(), 0);
}

#[test]
fn fallback_rejects_linked_scratch_without_writing_through_it() {
    let temp = tempdir();
    let input = inputs(&temp, Observation::Disabled);
    let mode_root = input
        .short_base
        .join(format!("fb-{}", path_hash(&input.home)))
        .join("prod");
    fs::ensure_private_directory(&mode_root).unwrap();
    let outside = temp.path().join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    if !link_fixture(&outside, &mode_root.join("tmp")) {
        return;
    }
    assert!(resolve(input).is_err());
    assert_eq!(std::fs::read_dir(outside).unwrap().count(), 0);
}

#[test]
fn enabled_policy_rejects_linked_retained_cache_before_reusing_it() {
    let temp = tempdir();
    let first = resolve(inputs(&temp, Observation::Disabled)).unwrap();
    let saved = temp.path().join("saved-cache");
    std::fs::rename(&first.cache_root, &saved).unwrap();
    let outside = temp.path().join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    if !link_fixture(&outside, &first.cache_root) {
        return;
    }
    assert!(resolve(inputs(&temp, Observation::Enabled)).is_err());
    assert_eq!(std::fs::read_dir(outside).unwrap().count(), 0);
}

#[test]
fn explicit_cache_rejects_linked_sidecars_without_writing_through_them() {
    for name in ["t", "b"] {
        let temp = tempdir();
        let mut input = inputs(&temp, Observation::Disabled);
        let configured = temp.path().join("explicit");
        let outside = temp.path().join("outside");
        std::fs::create_dir_all(&configured).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        if !link_fixture(&outside, &configured.join(name)) {
            return;
        }
        input.cache_override = Some(configured);
        assert!(resolve(input).is_err());
        assert_eq!(std::fs::read_dir(outside).unwrap().count(), 0);
    }
}

#[test]
fn fallback_failure_does_not_continue_with_deep_paths() {
    let temp = tempdir();
    let mut input = inputs(&temp, Observation::Disabled);
    input.short_base = temp.path().join("blocked-base");
    std::fs::write(&input.short_base, b"not a directory").unwrap();
    input.cache_override = Some(temp.path().join("long".repeat(30)));
    let error = resolve(input).unwrap_err().to_string();
    assert!(error.contains("fbuild --fix-win"));
    assert!(error.contains("short-path"));
}

#[test]
fn marker_write_failure_is_reported() {
    let temp = tempdir();
    let input = inputs(&temp, Observation::Disabled);
    std::fs::create_dir_all(input.fbuild_root.parent().unwrap()).unwrap();
    std::fs::write(&input.fbuild_root, b"not a directory").unwrap();
    assert!(resolve(input).is_err());
}

#[test]
fn oversized_short_base_is_rejected() {
    let temp = tempdir();
    let mut input = inputs(&temp, Observation::Disabled);
    input.short_base = temp.path().join("too-long".repeat(15));
    assert!(resolve(input).is_err());
}

#[test]
fn legacy_path_gate_is_active_only_while_fallback_is_needed() {
    let temp = tempdir();
    let deep = temp.path().join("long".repeat(70));
    let fallback = resolve(inputs(&temp, Observation::Disabled)).unwrap();
    assert!(fallback.validate_legacy_path(&deep).is_err());
    let enabled = resolve(inputs(&temp, Observation::Enabled)).unwrap();
    assert!(enabled.validate_legacy_path(&deep).is_ok());
}

#[test]
fn non_windows_resolution_does_not_create_fallback_or_warning() {
    let temp = tempdir();
    let input = inputs(&temp, Observation::NotApplicable);
    let expected = input.fbuild_root.join("cache");
    let resolution = resolve(input).unwrap();
    assert_eq!(resolution.cache_root, expected);
    assert!(resolution.short_build_root.is_none());
    assert!(resolution.advisory().is_none());
    assert_eq!(std::fs::read_dir(temp.path()).unwrap().count(), 0);
}
