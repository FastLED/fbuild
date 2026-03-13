//! Test utilities and fixtures for fbuild.

/// Create a temporary project directory with a minimal platformio.ini.
pub fn create_test_project(env_name: &str, platform: &str, board: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("failed to create temp dir");
    let ini_content =
        format!("[env:{env_name}]\nplatform = {platform}\nboard = {board}\nframework = arduino\n");
    std::fs::write(dir.path().join("platformio.ini"), ini_content)
        .expect("failed to write platformio.ini");
    std::fs::create_dir_all(dir.path().join("src")).expect("failed to create src/");
    std::fs::write(
        dir.path().join("src/main.cpp"),
        "#include <Arduino.h>\nvoid setup() {}\nvoid loop() {}\n",
    )
    .expect("failed to write main.cpp");
    dir
}
