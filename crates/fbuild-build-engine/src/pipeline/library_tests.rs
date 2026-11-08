use super::*;

#[test]
fn framework_archive_sources_honor_manifest_and_global_precedence() {
    let root = tempfile::tempdir().unwrap();
    let libraries: Vec<_> = [("direct", false), ("archived", true)]
        .into_iter()
        .map(|(name, archive)| {
            let dir = root.path().join(name);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(
                dir.join("library.json"),
                format!(r#"{{"build":{{"libArchive":{archive}}}}}"#),
            )
            .unwrap();
            fbuild_packages::library::FrameworkLibrary {
                name: name.into(),
                source_files: vec![dir.join("source.cpp")],
                dir,
                include_dirs: Vec::new(),
            }
        })
        .collect();
    let selected: Vec<_> = libraries
        .iter()
        .flat_map(|library| library.source_files.clone())
        .collect();
    assert_eq!(
        archive_eligible_library_sources(&libraries, &selected[1..], None).unwrap(),
        selected[1..]
    );
    assert_eq!(
        archive_eligible_library_sources(&libraries, &selected, None).unwrap(),
        libraries[1].source_files
    );
    assert!(
        archive_eligible_library_sources(&libraries, &selected, Some(false))
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        archive_eligible_library_sources(&libraries, &selected, Some(true)).unwrap(),
        selected
    );
}

#[test]
fn discovers_direct_library_root() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("library.properties"), "name=FastLED\n").unwrap();
    std::fs::create_dir_all(tmp.path().join("src")).unwrap();

    let roots = discover_extra_library_roots(tmp.path(), &[".".to_string()]);

    assert_eq!(roots.len(), 1);
    assert_eq!(roots[0], NormalizedPath::new(tmp.path()).into_path_buf());
}

#[test]
fn discovers_libraries_inside_storage_dir() {
    let tmp = tempfile::tempdir().unwrap();
    let storage = tmp.path().join("libs");
    let lib_a = storage.join("LibA");
    let lib_b = storage.join("LibB");
    std::fs::create_dir_all(lib_a.join("src")).unwrap();
    std::fs::create_dir_all(lib_b.join("src")).unwrap();

    let roots = discover_extra_library_roots(tmp.path(), &["libs".to_string()]);

    assert_eq!(roots.len(), 2);
    assert!(roots.iter().any(|root| root.ends_with("LibA")));
    assert!(roots.iter().any(|root| root.ends_with("LibB")));
}

#[test]
fn adds_src_and_root_include_dirs() {
    let tmp = tempfile::tempdir().unwrap();
    let lib = tmp.path().join("FastLED");
    std::fs::create_dir_all(lib.join("src")).unwrap();
    std::fs::write(lib.join("src").join("FastLED.h"), "").unwrap();

    let mut include_dirs = Vec::new();
    add_extra_library_include_dirs(std::slice::from_ref(&lib), &mut include_dirs).unwrap();

    assert!(include_dirs.contains(&lib.join("src")));
    assert!(include_dirs.contains(&lib));
}
