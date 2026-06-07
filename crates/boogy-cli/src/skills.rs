//! `boogy skills` — vendor the Boogy skills into a project so coding
//! agents discover them natively (Claude Code auto-discovers
//! `.claude/skills/`); no plugin required. `install` and `update` are
//! the same operation: fetch the current skills and replace the vendored
//! copy.

use anyhow::{bail, Context};
use std::fs;
use std::path::{Path, PathBuf};

const SKILLS_REPO: &str = "https://github.com/Boogy-ai/boogy-superpowers";
const DEFAULT_DEST: &str = ".claude/skills/boogy";
const MARKER_FILE: &str = ".boogy-skills";
const MARKER_CONTENT: &str =
    "vendored by 'boogy skills install' — this directory is replaced wholesale on update\n";

pub fn run(dest: Option<&str>, verb: &str) -> anyhow::Result<()> {
    let dest = PathBuf::from(dest.unwrap_or(DEFAULT_DEST));
    let tmp = std::env::temp_dir().join(format!("boogy-skills-{}", std::process::id()));
    let _ = fs::remove_dir_all(&tmp);

    let status = std::process::Command::new("git")
        .args(["clone", "--depth", "1", SKILLS_REPO])
        .arg(&tmp)
        .status()
        .context("running git clone (is git installed and on PATH?)")?;
    if !status.success() {
        bail!("git clone of {SKILLS_REPO} failed");
    }

    let n = vendor_skills(&tmp.join("skills"), &dest)?;
    let _ = fs::remove_dir_all(&tmp);
    println!("{verb} {n} skills into {}", dest.display());
    Ok(())
}

/// Replace `dest` with a copy of every skill directory under `src`.
/// Returns the number of skills copied.
fn vendor_skills(src: &Path, dest: &Path) -> anyhow::Result<usize> {
    if !src.is_dir() {
        bail!("no skills/ directory found in the fetched repo");
    }
    if dest.exists() {
        // A pre-existing dest is only removable if it is empty or was
        // created by a previous `boogy skills install` (marker present).
        let is_empty = dest.is_dir() && fs::read_dir(dest)?.next().is_none();
        let has_marker = dest.join(MARKER_FILE).is_file();
        if !is_empty && !has_marker {
            bail!(
                "refusing to replace '{}': it wasn't created by 'boogy skills install' \
                 (missing .boogy-skills marker) and is not empty. Choose an empty --dest \
                 or remove it yourself.",
                dest.display()
            );
        }
        fs::remove_dir_all(dest).with_context(|| format!("clearing {}", dest.display()))?;
    }
    fs::create_dir_all(dest)?;
    let mut n = 0;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            copy_dir(&entry.path(), &dest.join(entry.file_name()))?;
            n += 1;
        }
    }
    fs::write(dest.join(MARKER_FILE), MARKER_CONTENT)
        .with_context(|| format!("writing marker to {}", dest.display()))?;
    Ok(n)
}

fn copy_dir(src: &Path, dest: &Path) -> anyhow::Result<()> {
    fs::create_dir_all(dest)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let to = dest.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&entry.path(), &to)?;
        } else {
            fs::copy(entry.path(), &to)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vendor_skills_replaces_dest_and_counts_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("skills");
        fs::create_dir_all(src.join("using-boogy")).unwrap();
        fs::write(src.join("using-boogy/SKILL.md"), "x").unwrap();
        fs::create_dir_all(src.join("boogy-auth/sub")).unwrap();
        fs::write(src.join("boogy-auth/sub/ref.md"), "y").unwrap();

        let dest = tmp.path().join("out");
        // Pre-existing stale content must be replaced, not merged.
        // Give dest the ownership marker so the guard accepts it.
        fs::create_dir_all(&dest).unwrap();
        fs::write(dest.join(MARKER_FILE), MARKER_CONTENT).unwrap();
        fs::create_dir_all(dest.join("stale-skill")).unwrap();

        let n = vendor_skills(&src, &dest).unwrap();
        assert_eq!(n, 2);
        assert!(dest.join("using-boogy/SKILL.md").exists());
        assert!(dest.join("boogy-auth/sub/ref.md").exists());
        assert!(!dest.join("stale-skill").exists());
    }

    #[test]
    fn vendor_skills_errors_without_src() {
        let tmp = tempfile::tempdir().unwrap();
        let err = vendor_skills(&tmp.path().join("missing"), &tmp.path().join("out"));
        assert!(err.is_err());
    }

    #[test]
    fn vendor_skills_refuses_unmarked_nonempty_dest() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("skills");
        fs::create_dir_all(src.join("skill-a")).unwrap();
        fs::write(src.join("skill-a/SKILL.md"), "a").unwrap();

        let dest = tmp.path().join("out");
        fs::create_dir_all(&dest).unwrap();
        // A stray file with no marker — guard must refuse.
        fs::write(dest.join("precious.txt"), "keep me").unwrap();

        let err = vendor_skills(&src, &dest).unwrap_err();
        assert!(
            err.to_string().contains("refusing to replace"),
            "unexpected error: {err}"
        );
        // The precious file must still exist — nothing was deleted.
        assert!(dest.join("precious.txt").exists());
    }

    #[test]
    fn vendor_skills_replaces_marked_dest() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("skills");
        fs::create_dir_all(src.join("new-skill")).unwrap();
        fs::write(src.join("new-skill/SKILL.md"), "new").unwrap();

        let dest = tmp.path().join("out");
        fs::create_dir_all(&dest).unwrap();
        // Simulate a previous install: marker + old content.
        fs::write(dest.join(MARKER_FILE), MARKER_CONTENT).unwrap();
        fs::create_dir_all(dest.join("old-skill")).unwrap();
        fs::write(dest.join("old-skill/SKILL.md"), "old").unwrap();

        vendor_skills(&src, &dest).unwrap();

        assert!(!dest.join("old-skill").exists(), "old content should be gone");
        assert!(dest.join("new-skill/SKILL.md").exists(), "new skill should be present");
        assert!(dest.join(MARKER_FILE).exists(), "marker should be re-written");
    }

    #[test]
    fn vendor_skills_accepts_empty_dest_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("skills");
        fs::create_dir_all(src.join("skill-x")).unwrap();
        fs::write(src.join("skill-x/SKILL.md"), "x").unwrap();

        // Pre-created empty dest — no marker needed for an empty dir.
        let dest = tmp.path().join("out");
        fs::create_dir_all(&dest).unwrap();

        vendor_skills(&src, &dest).unwrap();

        assert!(dest.join("skill-x/SKILL.md").exists());
        assert!(dest.join(MARKER_FILE).exists(), "marker should be written after install");
    }
}
