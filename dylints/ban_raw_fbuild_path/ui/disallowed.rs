use std::path::Path;

fn main() {
    let project_dir = Path::new(".");
    let _build_dir = project_dir.join(".fbuild").join("build");
    let _firmware = format!(".fbuild/build/{}/release/firmware.bin", "uno");
}
