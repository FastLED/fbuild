//! Serialization, container, and on-disk write tests.

use crate::compile_database::{is_library_project, CompileDatabase, CompileEntry};

// --- Serialization tests ---

#[test]
fn test_compile_entry_serialization() {
    let entry = CompileEntry {
        arguments: vec![
            "/usr/bin/gcc".to_string(),
            "-c".to_string(),
            "main.c".to_string(),
        ],
        directory: "/project".to_string(),
        file: "main.c".to_string(),
        output: Some("main.o".to_string()),
    };

    let json = serde_json::to_value(&entry).unwrap();
    assert_eq!(json["directory"], "/project");
    assert_eq!(json["file"], "main.c");
    assert_eq!(json["output"], "main.o");
    assert!(json["arguments"].is_array());
}

#[test]
fn test_compile_entry_output_none_omitted() {
    let entry = CompileEntry {
        arguments: vec!["/usr/bin/gcc".to_string()],
        directory: "/project".to_string(),
        file: "main.c".to_string(),
        output: None,
    };

    let json = serde_json::to_string(&entry).unwrap();
    assert!(!json.contains("output"));
}

// --- CompileDatabase container tests ---

#[test]
fn test_database_empty() {
    let db = CompileDatabase::new();
    assert!(!db.has_entries());
}

#[test]
fn test_database_add_entry() {
    let mut db = CompileDatabase::new();
    db.add_entry(CompileEntry {
        arguments: vec![],
        directory: String::new(),
        file: "test.c".to_string(),
        output: None,
    });
    assert!(db.has_entries());
}

#[test]
fn test_database_write_valid_json() {
    let tmp = tempfile::TempDir::new().unwrap();
    let mut db = CompileDatabase::new();
    db.add_entry(CompileEntry {
        arguments: vec!["/usr/bin/gcc".to_string(), "-c".to_string()],
        directory: "/project".to_string(),
        file: "main.c".to_string(),
        output: None,
    });

    let path = db.write(tmp.path()).unwrap();
    assert!(path.exists());

    let content = std::fs::read_to_string(&path).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
    assert!(parsed.is_array());
    assert_eq!(parsed.as_array().unwrap().len(), 1);
    assert_eq!(parsed[0]["file"], "main.c");
}

#[test]
fn test_database_write_creates_parent_dirs() {
    let tmp = tempfile::TempDir::new().unwrap();
    let nested = tmp.path().join("a").join("b").join("c");
    let db = CompileDatabase::new();
    let path = db.write(&nested).unwrap();
    assert!(path.exists());
}

#[test]
fn test_database_write_and_copy() {
    let tmp = tempfile::TempDir::new().unwrap();
    let build_dir = tmp.path().join("build");
    let project_dir = tmp.path().join("project");

    let mut db = CompileDatabase::new();
    db.add_entry(CompileEntry {
        arguments: vec![],
        directory: String::new(),
        file: "test.c".to_string(),
        output: None,
    });

    db.write_and_copy(&build_dir, &project_dir).unwrap();
    assert!(build_dir.join("compile_commands.json").exists());
    assert!(project_dir.join("compile_commands.json").exists());
}

#[test]
fn test_database_write_does_not_rewrite_unchanged_contents() {
    let tmp = tempfile::TempDir::new().unwrap();
    let dir = tmp.path().join("build");
    let mut db = CompileDatabase::new();
    db.add_entry(CompileEntry {
        arguments: vec!["/usr/bin/gcc".to_string(), "-c".to_string()],
        directory: "/project".to_string(),
        file: "main.c".to_string(),
        output: None,
    });

    let path = db.write(&dir).unwrap();
    let first_mtime = std::fs::metadata(&path).unwrap().modified().unwrap();
    std::thread::sleep(std::time::Duration::from_millis(20));

    let path_again = db.write(&dir).unwrap();
    let second_mtime = std::fs::metadata(&path_again).unwrap().modified().unwrap();

    assert_eq!(path, path_again);
    assert_eq!(first_mtime, second_mtime);
}

#[test]
fn test_expected_output_path_prefers_project_root_for_normal_projects() {
    let tmp = tempfile::TempDir::new().unwrap();
    let build_dir = tmp.path().join("build");
    let project_dir = tmp.path().join("project");
    std::fs::create_dir_all(&project_dir).unwrap();

    assert_eq!(
        CompileDatabase::expected_output_path(&build_dir, &project_dir),
        project_dir.join("compile_commands.json")
    );
}

#[test]
fn test_expected_output_path_prefers_build_dir_for_library_projects() {
    let tmp = tempfile::TempDir::new().unwrap();
    let build_dir = tmp.path().join("build");
    let project_dir = tmp.path().join("project");
    std::fs::create_dir_all(&project_dir).unwrap();
    std::fs::write(project_dir.join("library.json"), r#"{"name":"test"}"#).unwrap();

    assert_eq!(
        CompileDatabase::expected_output_path(&build_dir, &project_dir),
        build_dir.join("compile_commands.json")
    );
}

// --- write_and_copy: both files must have identical content ---

#[test]
fn test_write_and_copy_identical_content() {
    let tmp = tempfile::TempDir::new().unwrap();
    let build_dir = tmp.path().join("build");
    let project_dir = tmp.path().join("project");

    let mut db = CompileDatabase::new();
    db.add_entry(CompileEntry {
        arguments: vec![
            "/usr/bin/g++".to_string(),
            "-c".to_string(),
            "main.cpp".to_string(),
        ],
        directory: "/project".to_string(),
        file: "main.cpp".to_string(),
        output: Some("main.cpp.o".to_string()),
    });

    db.write_and_copy(&build_dir, &project_dir).unwrap();

    let build_content =
        std::fs::read_to_string(build_dir.join("compile_commands.json")).unwrap();
    let project_content =
        std::fs::read_to_string(project_dir.join("compile_commands.json")).unwrap();
    assert_eq!(build_content, project_content);
}

// --- write_and_copy: suppressed when library.json exists ---

#[test]
fn test_write_and_copy_suppressed_for_library_project() {
    let tmp = tempfile::TempDir::new().unwrap();
    let build_dir = tmp.path().join("build");
    let project_dir = tmp.path().join("project");
    std::fs::create_dir_all(&project_dir).unwrap();

    // Create library.json to simulate a library project (like FastLED)
    std::fs::write(
        project_dir.join("library.json"),
        r#"{"name": "FastLED", "version": "3.10.3"}"#,
    )
    .unwrap();

    let mut db = CompileDatabase::new();
    db.add_entry(CompileEntry {
        arguments: vec!["/usr/bin/g++".to_string()],
        directory: project_dir.to_string_lossy().to_string(),
        file: "main.cpp".to_string(),
        output: None,
    });

    let result_path = db.write_and_copy(&build_dir, &project_dir).unwrap();

    // Build dir should have the file
    assert!(build_dir.join("compile_commands.json").exists());
    // Project dir should NOT have compile_commands.json (suppressed)
    assert!(
        !project_dir.join("compile_commands.json").exists(),
        "compile_commands.json should NOT be copied to project root for library projects"
    );
    // The returned path should be the build dir path
    assert_eq!(result_path, build_dir.join("compile_commands.json"));
}

#[test]
fn test_write_and_copy_not_suppressed_for_sketch_project() {
    let tmp = tempfile::TempDir::new().unwrap();
    let build_dir = tmp.path().join("build");
    let project_dir = tmp.path().join("project");
    std::fs::create_dir_all(&project_dir).unwrap();

    // No library.json — this is a normal sketch project
    let mut db = CompileDatabase::new();
    db.add_entry(CompileEntry {
        arguments: vec!["/usr/bin/g++".to_string()],
        directory: project_dir.to_string_lossy().to_string(),
        file: "main.cpp".to_string(),
        output: None,
    });

    db.write_and_copy(&build_dir, &project_dir).unwrap();

    // Both should exist
    assert!(build_dir.join("compile_commands.json").exists());
    assert!(project_dir.join("compile_commands.json").exists());
}

// --- is_library_project detection ---

#[test]
fn test_is_library_project_with_library_json() {
    let tmp = tempfile::TempDir::new().unwrap();
    std::fs::write(tmp.path().join("library.json"), r#"{"name": "MyLib"}"#).unwrap();
    assert!(is_library_project(tmp.path()));
}

#[test]
fn test_is_library_project_without_library_json() {
    let tmp = tempfile::TempDir::new().unwrap();
    assert!(!is_library_project(tmp.path()));
}

// --- Empty database produces valid JSON ---

#[test]
fn test_write_empty_database_valid_json() {
    let tmp = tempfile::TempDir::new().unwrap();
    let db = CompileDatabase::new();
    let path = db.write(tmp.path()).unwrap();

    let content = std::fs::read_to_string(&path).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
    assert!(parsed.is_array());
    assert!(parsed.as_array().unwrap().is_empty());
}

// --- Extend adds all entries ---

#[test]
fn test_database_extend_accumulates() {
    let mut db = CompileDatabase::new();
    let entries1 = vec![CompileEntry {
        arguments: vec![],
        directory: String::new(),
        file: "a.c".to_string(),
        output: None,
    }];
    let entries2 = vec![
        CompileEntry {
            arguments: vec![],
            directory: String::new(),
            file: "b.c".to_string(),
            output: None,
        },
        CompileEntry {
            arguments: vec![],
            directory: String::new(),
            file: "c.c".to_string(),
            output: None,
        },
    ];
    db.extend(entries1);
    db.extend(entries2);
    // Should have all 3 entries
    let tmp = tempfile::TempDir::new().unwrap();
    let path = db.write(tmp.path()).unwrap();
    let content = std::fs::read_to_string(&path).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
    assert_eq!(parsed.as_array().unwrap().len(), 3);
}
