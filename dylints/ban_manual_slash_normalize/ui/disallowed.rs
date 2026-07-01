use std::path::PathBuf;

fn main() {
    let path = PathBuf::from(r"C:\foo\bar\baz.cpp");
    let _arg = path.to_string_lossy().replace('\\', "/");
}
