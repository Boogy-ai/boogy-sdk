//! `boogy skills` — vendor the Boogy skills into a project so coding
//! agents discover them natively. The skill FILES are always vendored to
//! `.claude/skills/boogy` (Claude Code auto-discovers `.claude/skills/`).
//! For agents that don't auto-discover skills (Codex, Gemini, …), `--for`
//! also writes a managed pointer block into the agent's instruction file
//! (`AGENTS.md` / `GEMINI.md`) so they're routed to the entry skill.
//! `install` and `update` are the same operation: fetch the current skills
//! and replace the vendored copy.

use anyhow::{bail, Context};
use std::fs;
use std::path::{Path, PathBuf};

const SKILLS_REPO: &str = "https://github.com/Boogy-ai/boogy-superpowers";
const DEFAULT_DEST: &str = ".claude/skills/boogy";
const MARKER_FILE: &str = ".boogy-skills";
const MARKER_CONTENT: &str =
    "vendored by 'boogy skills install' — this directory is replaced wholesale on update\n";

/// Delimiters of the managed pointer block written into agent-instruction
/// files. Idempotent: re-running replaces the content between them rather
/// than appending a duplicate, and never touches the user's other content.
const POINTER_BEGIN: &str = "<!-- BEGIN BOOGY SKILLS (managed by `boogy skills install`) -->";
const POINTER_END: &str = "<!-- END BOOGY SKILLS -->";

/// Which coding agent(s) to wire up. The skill FILES are always vendored to
/// `dest`; this selects which agent-instruction files ALSO get a managed
/// "read the Boogy skills" pointer block so non-Claude agents discover them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, clap::ValueEnum)]
pub enum AgentTarget {
    /// Claude Code — auto-discovers `.claude/skills/`, so vendoring is enough (default).
    #[default]
    Claude,
    /// Codex / any `AGENTS.md`-reading agent — also write an `AGENTS.md` pointer.
    Codex,
    /// Gemini CLI — also write a `GEMINI.md` pointer.
    Gemini,
    /// Write both `AGENTS.md` and `GEMINI.md` pointers.
    All,
    /// Detect the project's agents and write the right pointers (`AGENTS.md`
    /// always — the universal standard — plus `GEMINI.md` if Gemini is used).
    Auto,
}

pub fn run(dest: Option<&str>, verb: &str, target: AgentTarget) -> anyhow::Result<()> {
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

    // Write agent-instruction pointers for non-Claude targets so they're
    // routed to the entry skill (Claude needs none — it auto-discovers the dir).
    let root = Path::new(".");
    for path in pointer_targets(target, root) {
        write_pointer_file(&path, &dest)?;
        println!("pointed {} at the Boogy skills", path.display());
    }
    Ok(())
}

/// Which agent-instruction files to write a pointer into, for `target`,
/// relative to the project `root`.
fn pointer_targets(target: AgentTarget, root: &Path) -> Vec<PathBuf> {
    let agents = root.join("AGENTS.md");
    let gemini = root.join("GEMINI.md");
    match target {
        AgentTarget::Claude => vec![],
        AgentTarget::Codex => vec![agents],
        AgentTarget::Gemini => vec![gemini],
        AgentTarget::All => vec![agents, gemini],
        AgentTarget::Auto => {
            // AGENTS.md is the cross-agent standard (Codex, Cursor, …) and is
            // harmless for Claude — always write it. Add GEMINI.md only when
            // the project already uses Gemini, to avoid surprising files.
            let mut v = vec![agents];
            if gemini.exists() || root.join(".gemini").exists() {
                v.push(gemini);
            }
            v
        }
    }
}

/// Read `path` (or empty), upsert the managed Boogy pointer block referencing
/// `skills_dir`, and write it back.
fn write_pointer_file(path: &Path, skills_dir: &Path) -> anyhow::Result<()> {
    let existing = fs::read_to_string(path).unwrap_or_default();
    let updated = upsert_managed_block(&existing, &pointer_block(skills_dir));
    fs::write(path, updated).with_context(|| format!("writing pointer to {}", path.display()))?;
    Ok(())
}

/// The managed pointer block, referencing the vendored entry skill at
/// `skills_dir`. Tells any markdown-reading agent to use the skills first.
fn pointer_block(skills_dir: &Path) -> String {
    let entry = skills_dir.join("using-boogy/SKILL.md");
    format!(
        "{POINTER_BEGIN}\n\
## Building on Boogy\n\
\n\
This project deploys to **Boogy**, a WebAssembly backend platform. Before you\n\
write or change any Boogy service, frontend, or `boogy.toml` manifest, read the\n\
vendored Boogy skills — start at the entry point, which routes every task to the\n\
right skill and runs a design-first gate:\n\
\n\
    {entry}\n\
\n\
Do not build Boogy from memory: the skills encode the platform's hard invariants\n\
(transactions, data integrity, capability-based security, the high-level SDK\n\
interfaces) and cover service design, data modeling, auth, serving frontends,\n\
websockets, MCP/REST surfaces, background jobs, and deploy.\n\
{POINTER_END}",
        entry = entry.display(),
    )
}

/// Insert `block` into `existing`, or replace a prior managed block in place.
/// Idempotent: running twice yields the same result with exactly one block.
/// Content outside the BEGIN/END markers is never modified.
fn upsert_managed_block(existing: &str, block: &str) -> String {
    if let (Some(start), Some(end_at)) = (existing.find(POINTER_BEGIN), existing.find(POINTER_END)) {
        let end = end_at + POINTER_END.len();
        if end > start {
            let mut out = String::with_capacity(existing.len());
            out.push_str(&existing[..start]);
            out.push_str(block);
            out.push_str(&existing[end..]);
            return out;
        }
    }
    // No prior block — append, keeping a blank line between existing content
    // and ours.
    let mut out = existing.to_string();
    if !out.is_empty() {
        if !out.ends_with('\n') {
            out.push('\n');
        }
        out.push('\n');
    }
    out.push_str(block);
    out.push('\n');
    out
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
    fn upsert_inserts_block_when_absent_preserving_content() {
        let existing = "# My project\n\nSome notes about the repo.\n";
        let out = upsert_managed_block(existing, "BLOCK");
        assert!(out.starts_with(existing), "user content must be preserved verbatim at the top");
        assert!(out.contains("BLOCK"));
        // A blank line separates the user's content from ours.
        assert!(out.contains("repo.\n\nBLOCK"));
    }

    #[test]
    fn upsert_into_empty_file_is_just_the_block() {
        let out = upsert_managed_block("", "BLOCK");
        assert_eq!(out, "BLOCK\n");
    }

    #[test]
    fn upsert_is_idempotent() {
        let block = pointer_block(Path::new(".claude/skills/boogy"));
        let once = upsert_managed_block("# Proj\n", &block);
        let twice = upsert_managed_block(&once, &block);
        assert_eq!(once, twice, "re-running must not change a file that already has the block");
        assert_eq!(
            once.matches(POINTER_BEGIN).count(),
            1,
            "exactly one managed block, never a duplicate",
        );
    }

    #[test]
    fn upsert_replaces_prior_block_preserving_surroundings() {
        let existing = format!("TOP CONTENT\n\n{POINTER_BEGIN}\nstale\n{POINTER_END}\n\nBOTTOM CONTENT\n");
        let fresh = format!("{POINTER_BEGIN}\nfresh\n{POINTER_END}");
        let out = upsert_managed_block(&existing, &fresh);
        assert!(out.contains("TOP CONTENT"), "content before the block is kept");
        assert!(out.contains("BOTTOM CONTENT"), "content after the block is kept");
        assert!(out.contains("fresh") && !out.contains("stale"), "block content is replaced");
        assert_eq!(out.matches(POINTER_BEGIN).count(), 1);
    }

    #[test]
    fn pointer_block_references_the_entry_skill() {
        let block = pointer_block(Path::new(".claude/skills/boogy"));
        assert!(block.contains(".claude/skills/boogy/using-boogy/SKILL.md"));
        assert!(block.starts_with(POINTER_BEGIN));
        assert!(block.ends_with(POINTER_END));
    }

    #[test]
    fn pointer_targets_per_agent() {
        let root = Path::new("/no/such/project/root");
        assert!(pointer_targets(AgentTarget::Claude, root).is_empty(), "claude needs no pointer");
        assert_eq!(pointer_targets(AgentTarget::Codex, root), vec![root.join("AGENTS.md")]);
        assert_eq!(pointer_targets(AgentTarget::Gemini, root), vec![root.join("GEMINI.md")]);
        assert_eq!(
            pointer_targets(AgentTarget::All, root),
            vec![root.join("AGENTS.md"), root.join("GEMINI.md")],
        );
        // auto on a project with no Gemini footprint → AGENTS.md only.
        assert_eq!(pointer_targets(AgentTarget::Auto, root), vec![root.join("AGENTS.md")]);
    }

    #[test]
    fn pointer_targets_auto_adds_gemini_when_present() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("GEMINI.md"), "# existing gemini config\n").unwrap();
        assert_eq!(
            pointer_targets(AgentTarget::Auto, tmp.path()),
            vec![tmp.path().join("AGENTS.md"), tmp.path().join("GEMINI.md")],
            "auto must add GEMINI.md when the project already uses Gemini",
        );
    }

    #[test]
    fn write_pointer_file_creates_then_updates_idempotently() {
        let tmp = tempfile::tempdir().unwrap();
        let agents = tmp.path().join("AGENTS.md");
        fs::write(&agents, "# House rules\n\nUse tabs.\n").unwrap();
        let skills = Path::new(".claude/skills/boogy");

        write_pointer_file(&agents, skills).unwrap();
        let after_first = fs::read_to_string(&agents).unwrap();
        assert!(after_first.contains("# House rules"), "pre-existing content survives");
        assert!(after_first.contains("using-boogy/SKILL.md"));

        write_pointer_file(&agents, skills).unwrap();
        let after_second = fs::read_to_string(&agents).unwrap();
        assert_eq!(after_first, after_second, "second run is a no-op");
        assert_eq!(after_second.matches(POINTER_BEGIN).count(), 1);
    }

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
