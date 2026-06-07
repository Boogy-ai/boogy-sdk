/// Returns the path to the WIT directory for use in build scripts.
pub fn wit_dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("wit")
}
