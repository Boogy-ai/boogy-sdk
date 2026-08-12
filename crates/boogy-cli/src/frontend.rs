//! Frontend bundle packaging.
//!
//! A service may ship a frontend alongside (or instead of) its wasm. The
//! author points `[frontend].root` at a directory of source files; we tar +
//! gzip that directory and upload it as the `frontend` multipart field. The
//! host unpacks, transpiles, and serves it — no JS toolchain runs locally.
//!
//! The packaging here is intentionally generic: it walks a directory and
//! preserves each regular file's path relative to the root.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use flate2::write::GzEncoder;
use flate2::Compression;

/// Top-level directory names skipped when packaging a frontend root. These are
/// never part of a shippable bundle and can be enormous, so we keep them out of
/// the upload.
const SKIP_TOP_LEVEL: &[&str] = &["node_modules", ".git"];

/// Walk `root` recursively and produce a gzipped tar of every regular file
/// found, keyed by each file's path **relative to `root`** (forward-slash
/// separators, no leading `./`). A top-level `node_modules`/`.git` is skipped.
///
/// Symlinks are not followed (a symlinked directory is not descended into; a
/// symlink to a file is read as its target via the metadata `is_file` check on
/// the resolved entry — `walk` uses `std::fs::read_dir`, which yields the link).
pub fn tar_gz_dir(root: &Path) -> Result<Vec<u8>> {
    if !root.is_dir() {
        anyhow::bail!("frontend root is not a directory: {}", root.display());
    }

    let mut files: Vec<(String, PathBuf)> = Vec::new();
    collect_files(root, root, true, &mut files)?;
    // Stable, reproducible ordering regardless of filesystem iteration order.
    files.sort_by(|a, b| a.0.cmp(&b.0));

    let mut builder = tar::Builder::new(GzEncoder::new(Vec::new(), Compression::default()));
    for (rel, abs) in files {
        let mut f = std::fs::File::open(&abs)
            .with_context(|| format!("failed to open frontend file: {}", abs.display()))?;
        builder
            .append_file(&rel, &mut f)
            .with_context(|| format!("failed to add to bundle: {rel}"))?;
    }

    let gz = builder
        .into_inner()
        .context("failed to finalize frontend tar")?;
    let bytes = gz.finish().context("failed to gzip frontend tar")?;
    Ok(bytes)
}

/// Recursively collect regular files under `dir`, recording each as
/// `(relative_path_from_root, absolute_path)`. `top_level` flags whether `dir`
/// is the root itself, so we only skip `node_modules`/`.git` at depth 0.
fn collect_files(
    root: &Path,
    dir: &Path,
    top_level: bool,
    out: &mut Vec<(String, PathBuf)>,
) -> Result<()> {
    let entries = std::fs::read_dir(dir)
        .with_context(|| format!("failed to read directory: {}", dir.display()))?;
    for entry in entries {
        let entry = entry.context("failed to read directory entry")?;
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();

        let file_type = entry
            .file_type()
            .with_context(|| format!("failed to stat: {}", path.display()))?;

        if file_type.is_dir() {
            if top_level && SKIP_TOP_LEVEL.contains(&name.as_ref()) {
                continue;
            }
            collect_files(root, &path, false, out)?;
        } else if file_type.is_file() {
            let rel = path
                .strip_prefix(root)
                .expect("walked path is under root")
                .to_string_lossy()
                // Normalize to forward slashes for the tar entry name.
                .replace('\\', "/");
            out.push((rel, path));
        }
        // Symlinks and other special files are ignored.
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::io::Read;

    /// Unpack a gzipped tar into a path→contents map for assertions.
    fn untar_gz(bytes: &[u8]) -> BTreeMap<String, Vec<u8>> {
        let gz = flate2::read::GzDecoder::new(bytes);
        let mut archive = tar::Archive::new(gz);
        let mut out = BTreeMap::new();
        for entry in archive.entries().expect("entries") {
            let mut entry = entry.expect("entry");
            let path = entry.path().expect("path").to_string_lossy().into_owned();
            let mut buf = Vec::new();
            entry.read_to_end(&mut buf).expect("read entry");
            out.insert(path, buf);
        }
        out
    }

    #[test]
    fn tar_gz_dir_roundtrips_files_and_paths() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        std::fs::write(root.join("index.ts"), b"export const x = 1;").unwrap();
        std::fs::create_dir_all(root.join("components")).unwrap();
        std::fs::write(root.join("components/app.tsx"), b"<App/>").unwrap();

        let bytes = tar_gz_dir(root).expect("tar_gz_dir");
        let unpacked = untar_gz(&bytes);

        assert_eq!(unpacked.len(), 2, "two files round-trip");
        assert_eq!(
            unpacked.get("index.ts").map(|v| v.as_slice()),
            Some(b"export const x = 1;".as_slice())
        );
        assert_eq!(
            unpacked.get("components/app.tsx").map(|v| v.as_slice()),
            Some(b"<App/>".as_slice()),
            "nested path uses forward slashes, relative to root, no leading ./"
        );
    }

    #[test]
    fn tar_gz_dir_skips_node_modules_and_git_at_top_level() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        std::fs::write(root.join("main.js"), b"keep").unwrap();
        std::fs::create_dir_all(root.join("node_modules/pkg")).unwrap();
        std::fs::write(root.join("node_modules/pkg/index.js"), b"drop").unwrap();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::write(root.join(".git/config"), b"drop").unwrap();
        // A nested dir literally named node_modules is NOT skipped (only top-level).
        std::fs::create_dir_all(root.join("src/node_modules")).unwrap();
        std::fs::write(root.join("src/node_modules/keep.js"), b"keep").unwrap();

        let bytes = tar_gz_dir(root).expect("tar_gz_dir");
        let unpacked = untar_gz(&bytes);

        assert!(unpacked.contains_key("main.js"));
        assert!(unpacked.contains_key("src/node_modules/keep.js"));
        assert!(!unpacked.keys().any(|k| k.starts_with("node_modules/")));
        assert!(!unpacked.keys().any(|k| k.starts_with(".git/")));
    }

    #[test]
    fn tar_gz_dir_rejects_non_directory() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("not-a-dir");
        std::fs::write(&file, b"x").unwrap();
        assert!(tar_gz_dir(&file).is_err());
    }
}
