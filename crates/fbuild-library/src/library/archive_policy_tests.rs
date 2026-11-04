//! Native link acceptance for library archive policy.

use super::*;
use fbuild_core::subprocess::run_command;

#[tokio::test]
#[ignore = "#1885: requires native GCC and a GNU-compatible archiver"]
async fn manifest_disables_archiving_and_links_unreferenced_override() {
    exercise_archive_policy(Some(false), None, false, false).await;
}

#[tokio::test]
#[ignore = "#1885: requires native GCC and a GNU-compatible archiver"]
async fn environment_disables_archiving_even_when_manifest_enables_it() {
    exercise_archive_policy(Some(true), Some("no"), false, false).await;
}

#[tokio::test]
#[ignore = "#1885: requires native GCC and a GNU-compatible archiver"]
async fn explicit_environment_enables_archiving_over_manifest_false() {
    exercise_archive_policy(Some(false), Some("yes"), true, false).await;
}

#[tokio::test]
#[ignore = "#1885: requires native GCC and a GNU-compatible archiver"]
async fn absent_archive_policy_preserves_default_archive_extraction() {
    exercise_archive_policy(None, None, true, false).await;
}

#[tokio::test]
#[ignore = "#1885: requires native GCC and a GNU-compatible archiver"]
async fn cached_archive_is_not_linked_after_archiving_is_disabled() {
    exercise_archive_policy(Some(false), None, false, true).await;
}

async fn exercise_archive_policy(
    manifest: Option<bool>,
    environment: Option<&str>,
    archived: bool,
    warm_archive: bool,
) {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let library = root.join("override");
    std::fs::create_dir_all(library.join("src")).unwrap();
    std::fs::write(
        library.join("library.json"),
        match manifest {
            Some(value) => format!(r#"{{"name":"Override", "build":{{"libArchive":{value}}}}}"#),
            None => r#"{"name":"Override"}"#.into(),
        },
    )
    .unwrap();
    std::fs::write(
        library.join("src/anchor.c"),
        "int anchor(void) { return 3; }\n",
    )
    .unwrap();
    std::fs::write(
        library.join("src/override.c"),
        "int hook(void) { return 2; }\n",
    )
    .unwrap();
    let gcc = std::env::var("FBUILD_TEST_CC").unwrap_or_else(|_| "gcc".into());
    let ar = std::env::var("FBUILD_TEST_AR").unwrap_or_else(|_| "ar".into());
    let mut resolved = resolve_fixture(root, &gcc, &ar).await;
    if warm_archive {
        let cached = resolved
            .plan
            .with_archive_override(Some(true))
            .compile(&library_compiler::job_gate(2))
            .await
            .unwrap();
        assert!(
            cached[0]
                .extension()
                .is_some_and(|extension| extension == "a")
        );
        resolved = resolve_fixture(root, &gcc, &ar).await;
    }
    let ini = root.join("platformio.ini");
    std::fs::write(
        &ini,
        format!(
            "[env:probe]\nplatform=native\n{}",
            environment
                .map(|value| format!("lib_archive={value}\n"))
                .unwrap_or_default()
        ),
    )
    .unwrap();
    let config = fbuild_config::PlatformIOConfig::from_path(&ini).unwrap();
    let inputs = resolved
        .plan
        .with_archive_override(config.get_lib_archive("probe").unwrap())
        .compile(&library_compiler::job_gate(2))
        .await
        .unwrap();
    std::fs::write(
        root.join("main.c"),
        "__attribute__((weak)) int hook(void) { return 1; }\n\
         int main(void) { return hook() == 2 ? 0 : 1; }\n",
    )
    .unwrap();
    let executable = root.join("archive-policy-probe");
    let mut link = vec![
        gcc,
        "main.c".into(),
        "-o".into(),
        executable.display().to_string(),
    ];
    link.extend(inputs.iter().map(|input| input.display().to_string()));
    let args: Vec<&str> = link.iter().map(String::as_str).collect();
    let linked = run_command(&args, Some(root), None, None).await.unwrap();
    assert!(linked.success(), "{}", linked.stderr);
    let executed = run_command(&[&executable.display().to_string()], Some(root), None, None)
        .await
        .unwrap();
    assert_eq!(
        executed.exit_code,
        i32::from(archived),
        "direct objects must override the weak hook; archive extraction must preserve it; inputs: {inputs:?}"
    );
    let extension = if archived { "a" } else { "o" };
    assert!(
        inputs
            .iter()
            .all(|input| input.extension().is_some_and(|e| e == extension))
    );
    assert_eq!(
        inputs.len(),
        if archived { 1 } else { 2 },
        "every object must reach the linker"
    );
    assert_eq!(
        root.join("build/libs/override/liboverride.a").exists(),
        archived || warm_archive
    );
}

async fn resolve_fixture(root: &Path, gcc: &str, ar: &str) -> ResolvedLibraries {
    resolve_libraries(
        &["Override=symlink://override".into()],
        &[],
        Path::new(gcc),
        Path::new(gcc),
        Path::new(ar),
        &[],
        &[],
        &[],
        root,
        &root.join("build/libs"),
        false,
        None,
    )
    .await
    .unwrap()
}
