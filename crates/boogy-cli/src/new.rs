//! `boogy new` — start a frontend project from the template the platform
//! serves, so a new app always begins on the SDK version its host delivers.

use anyhow::{bail, Context};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum BuildMode {
    /// The platform builds your TypeScript at deploy. No local toolchain.
    Source,
    /// You build locally with Vite (live reload, faked platform); deploy the output.
    Dist,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TemplateBundle {
    pub sdk_version: String,
    pub files: BTreeMap<String, String>,
}

pub fn validate_name(name: &str) -> anyhow::Result<()> {
    let ok = !name.is_empty()
        && name.len() <= 63
        && name.chars().next().is_some_and(|c| c.is_ascii_lowercase())
        && name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    // Stricter than the platform's own segment rule (which also allows `_`
    // and uppercase): the name is also the npm package name and the mount
    // path, and a name that is valid in all three is the one worth teaching.
    if !ok {
        bail!("\"{name}\" is not a valid service name: use lowercase letters, digits and hyphens, starting with a letter (max 63)");
    }
    Ok(())
}

fn substitute(text: &str, vars: &[(&str, &str)]) -> anyhow::Result<String> {
    let mut out = text.to_string();
    for (k, v) in vars {
        out = out.replace(&format!("{{{{{k}}}}}"), v);
    }
    if let Some(start) = out.find("{{") {
        let end = out[start..].find("}}").map(|e| start + e + 2).unwrap_or(out.len());
        bail!("template placeholder {} has no value", &out[start..end]);
    }
    Ok(out)
}

pub fn compose(bundle: &TemplateBundle, mode: BuildMode, name: &str) -> anyhow::Result<BTreeMap<PathBuf, String>> {
    validate_name(name)?;
    let (mode_dir, src) = match mode {
        BuildMode::Source => ("source", "web"),
        BuildMode::Dist => ("dist", "src"),
    };
    let vars = [("name", name), ("sdk_version", bundle.sdk_version.as_str()), ("src", src)];
    let mut out = BTreeMap::new();
    for layer in ["common", mode_dir] {
        for (key, content) in &bundle.files {
            let Some(rest) = key.strip_prefix(&format!("{layer}/")) else { continue };
            let path = PathBuf::from(substitute(rest, &vars)?);
            if path.is_absolute() || path.components().any(|c| matches!(c, std::path::Component::ParentDir)) {
                bail!("template path {key} escapes the project directory");
            }
            out.insert(path, substitute(content, &vars)?);
        }
    }
    Ok(out)
}

pub fn read_template_dir(dir: &Path) -> anyhow::Result<TemplateBundle> {
    // The template sits inside the SDK package, so its version IS the
    // package's version: read it rather than keep a second copy that drifts.
    let pkg = dir.join("../package.json");
    let pkg_json: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(&pkg).with_context(|| format!("no package.json beside {}", dir.display()))?,
    )
    .with_context(|| format!("{} is not valid JSON", pkg.display()))?;
    let sdk_version = pkg_json["version"]
        .as_str()
        .with_context(|| format!("{} has no version", pkg.display()))?
        .to_string();
    let mut files = BTreeMap::new();
    fn walk(root: &Path, dir: &Path, files: &mut BTreeMap<String, String>) -> anyhow::Result<()> {
        for entry in std::fs::read_dir(dir)? {
            let path = entry?.path();
            if path.is_dir() {
                walk(root, &path, files)?;
            } else {
                let rel = path.strip_prefix(root)?.to_string_lossy().replace('\\', "/");
                files.insert(rel, std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?);
            }
        }
        Ok(())
    }
    walk(dir, dir, &mut files)?;
    Ok(TemplateBundle { sdk_version, files })
}

pub async fn fetch_template(host: &str) -> anyhow::Result<TemplateBundle> {
    let url = format!("{}/v1/web-template", host.trim_end_matches('/'));
    let resp = reqwest::get(&url).await.with_context(|| format!("could not reach {url}"))?;
    if !resp.status().is_success() {
        bail!("{url} answered {} — this host does not serve the project template yet", resp.status());
    }
    Ok(resp.json().await.context("the template response was not valid JSON")?)
}

pub fn write_project(dest: &Path, files: &BTreeMap<PathBuf, String>) -> anyhow::Result<()> {
    if dest.exists() && std::fs::read_dir(dest)?.next().is_some() {
        bail!("{} is not empty; choose a new directory", dest.display());
    }
    for (rel, content) in files {
        let path = dest.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, content)?;
    }
    Ok(())
}

/// Markers around the part of a project's `AGENTS.md` that `boogy new
/// --upgrade` owns. Everything outside them belongs to the developer.
pub const BLOCK_BEGIN: &str = "<!-- BEGIN BOOGY SCAFFOLD (managed by `boogy new --upgrade`; edit outside this block) -->";
pub const BLOCK_END: &str = "<!-- END BOOGY SCAFFOLD -->";

pub struct UpgradeReport {
    /// The `@boogy/web` version the old block named, if there was a block.
    pub from: Option<String>,
    pub to: String,
}

/// The version an `AGENTS.md` block records, read from its `@boogy/web X` text.
fn block_version(agents: &str) -> Option<String> {
    let start = agents.find(BLOCK_BEGIN)?;
    let end = agents[start..].find(BLOCK_END)? + start;
    let block = &agents[start..end];
    let at = block.find("@boogy/web ")? + "@boogy/web ".len();
    Some(block[at..].split_whitespace().next()?.trim_end_matches(['.', ',', '`']).to_string())
}

/// Refresh the scaffold-owned parts of an existing project. Today that is the
/// managed block in `AGENTS.md`; everything the scaffold used to copy in
/// (layout, dev platform, Vite and TypeScript setup) lives in `@boogy/web`
/// itself, so upgrading that dependency upgrades it. Developer files are never
/// rewritten.
pub fn upgrade(dir: &Path, bundle: &TemplateBundle) -> anyhow::Result<UpgradeReport> {
    let manifest = std::fs::read_to_string(dir.join("boogy.toml"))
        .with_context(|| format!("{} has no boogy.toml — run this in a project boogy new created", dir.display()))?;
    let doc: toml::Value = manifest.parse().context("boogy.toml is not valid TOML")?;
    let name = doc
        .get("service")
        .and_then(|s| s.get("id"))
        .and_then(|v| v.as_str())
        .context("boogy.toml has no [service] id")?
        .to_string();
    let mode = if dir.join("package.json").exists() { BuildMode::Dist } else { BuildMode::Source };
    let files = compose(bundle, mode, &name)?;
    let fresh = files.get(Path::new("AGENTS.md")).context("the template has no AGENTS.md")?;
    let block = fresh.trim_end_matches('\n');
    let path = dir.join("AGENTS.md");
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    let from = block_version(&existing);
    std::fs::write(&path, crate::skills::upsert_marked_block(&existing, block, BLOCK_BEGIN, BLOCK_END))?;
    Ok(UpgradeReport { from, to: bundle.sdk_version.clone() })
}

pub async fn run(name: String, mode: BuildMode, host: String, template_dir: Option<PathBuf>, dest: Option<PathBuf>) -> anyhow::Result<()> {
    let bundle = match template_dir {
        Some(dir) => read_template_dir(&dir)?,
        None => fetch_template(&host).await?,
    };
    let files = compose(&bundle, mode, &name)?;
    let dest = dest.unwrap_or_else(|| PathBuf::from(&name));
    write_project(&dest, &files)?;
    println!("Created {} ({} files, @boogy/web {}).", dest.display(), files.len(), bundle.sdk_version);
    println!("Next:");
    println!("  cd {}", dest.display());
    match mode {
        BuildMode::Source => {
            println!("  edit web/theme.css and web/app.tsx");
            println!("  boogy deploy boogy.toml --smoke");
        }
        BuildMode::Dist => {
            println!("  npm install && npm run dev");
            println!("  npm run build && boogy deploy boogy.toml --smoke");
        }
    }
    Ok(())
}

pub async fn run_upgrade(host: String, template_dir: Option<PathBuf>, dir: Option<PathBuf>) -> anyhow::Result<()> {
    let bundle = match template_dir {
        Some(t) => read_template_dir(&t)?,
        None => fetch_template(&host).await?,
    };
    let dir = dir.unwrap_or_else(|| PathBuf::from("."));
    let report = upgrade(&dir, &bundle)?;
    match report.from {
        Some(from) if from == report.to => println!("AGENTS.md scaffold block refreshed (already on @boogy/web {}).", report.to),
        Some(from) => println!("AGENTS.md scaffold block upgraded: @boogy/web {from} -> {}.", report.to),
        None => println!("AGENTS.md scaffold block added (@boogy/web {}).", report.to),
    }
    println!("Upgrade the library itself too: in dist mode bump @boogy/web in package.json; in source mode redeploy.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bundle() -> TemplateBundle {
        let mut files = BTreeMap::new();
        files.insert("common/AGENTS.md".into(), "# {{name}}\n".into());
        files.insert("common/{{src}}/theme.css".into(), ":root{}\n".into());
        files.insert("source/boogy.toml".into(), "id = \"{{name}}\" # sdk {{sdk_version}}\n".into());
        files.insert("dist/boogy.toml".into(), "id = \"{{name}}\"\nroot = \"dist\"\n".into());
        files.insert("dist/package.json".into(), "{\"name\":\"{{name}}\"}\n".into());
        TemplateBundle { sdk_version: "0.2.0".into(), files }
    }

    #[test]
    fn source_mode_takes_common_plus_source_and_substitutes_paths_and_contents() {
        let out = compose(&bundle(), BuildMode::Source, "myapp").unwrap();
        let keys: Vec<_> = out.keys().map(|p| p.to_string_lossy().to_string()).collect();
        assert_eq!(keys, vec!["AGENTS.md", "boogy.toml", "web/theme.css"]);
        assert_eq!(out[&PathBuf::from("boogy.toml")], "id = \"myapp\" # sdk 0.2.0\n");
    }

    #[test]
    fn dist_mode_uses_src_and_its_own_files() {
        let out = compose(&bundle(), BuildMode::Dist, "myapp").unwrap();
        assert!(out.contains_key(&PathBuf::from("src/theme.css")));
        assert!(out.contains_key(&PathBuf::from("package.json")));
        assert!(out[&PathBuf::from("boogy.toml")].contains("root = \"dist\""));
    }

    #[test]
    fn an_unknown_placeholder_is_an_error_not_shipped_literally() {
        let mut b = bundle();
        b.files.insert("common/x.txt".into(), "{{nope}}".into());
        assert!(compose(&b, BuildMode::Source, "myapp").is_err());
    }

    #[test]
    fn names_follow_the_service_id_rule() {
        assert!(validate_name("myapp").is_ok());
        assert!(validate_name("my-app2").is_ok());
        for bad in ["", "MyApp", "1app", "my_app", "a/b", ".."] {
            assert!(validate_name(bad).is_err(), "{bad} should be refused");
        }
    }

    #[test]
    fn refuses_a_non_empty_destination_and_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("keep.txt"), "mine").unwrap();
        let files = compose(&bundle(), BuildMode::Source, "myapp").unwrap();
        assert!(write_project(dir.path(), &files).is_err());
        assert_eq!(std::fs::read_to_string(dir.path().join("keep.txt")).unwrap(), "mine");
        assert!(!dir.path().join("boogy.toml").exists());
    }

    #[test]
    fn writes_into_an_empty_or_missing_destination() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("myapp");
        let files = compose(&bundle(), BuildMode::Source, "myapp").unwrap();
        write_project(&dest, &files).unwrap();
        assert!(dest.join("web/theme.css").exists());
    }

    #[test]
    fn reads_a_template_directory_into_the_same_keys_the_host_serves() {
        // Laid out like the SDK package: <pkg>/package.json + <pkg>/template/.
        let pkg = tempfile::tempdir().unwrap();
        std::fs::write(pkg.path().join("package.json"), r#"{"name":"@boogy/web","version":"0.2.0"}"#).unwrap();
        let dir = pkg.path().join("template");
        std::fs::create_dir_all(dir.join("common/{{src}}")).unwrap();
        std::fs::write(dir.join("common/{{src}}/theme.css"), ":root{}").unwrap();
        let b = read_template_dir(&dir).unwrap();
        assert_eq!(b.sdk_version, "0.2.0");
        assert_eq!(b.files.keys().collect::<Vec<_>>(), vec!["common/{{src}}/theme.css"]);
    }

    fn project_with_agents(dir: &Path, agents: &str) {
        std::fs::write(dir.join("boogy.toml"), "[service]\nid = \"myapp\"\n").unwrap();
        std::fs::write(dir.join("AGENTS.md"), agents).unwrap();
    }

    fn bundle_with_block(version: &str) -> TemplateBundle {
        let mut files = BTreeMap::new();
        files.insert(
            "common/AGENTS.md".into(),
            format!("{BLOCK_BEGIN}\n# {{{{name}}}} on @boogy/web {{{{sdk_version}}}}\n{BLOCK_END}\n"),
        );
        TemplateBundle { sdk_version: version.into(), files }
    }

    #[test]
    fn upgrade_replaces_only_the_managed_block() {
        let dir = tempfile::tempdir().unwrap();
        project_with_agents(
            dir.path(),
            &format!("My notes above.\n\n{BLOCK_BEGIN}\n# myapp on @boogy/web 0.1.0\n{BLOCK_END}\n\nMy notes below.\n"),
        );
        let report = upgrade(dir.path(), &bundle_with_block("0.2.0")).unwrap();
        let after = std::fs::read_to_string(dir.path().join("AGENTS.md")).unwrap();
        assert!(after.starts_with("My notes above.\n"), "{after}");
        assert!(after.contains("My notes below."), "{after}");
        assert!(after.contains("# myapp on @boogy/web 0.2.0"), "{after}");
        assert!(!after.contains("0.1.0"), "{after}");
        assert_eq!(after.matches(BLOCK_BEGIN).count(), 1);
        assert_eq!(report.from.as_deref(), Some("0.1.0"));
        assert_eq!(report.to, "0.2.0");
    }

    #[test]
    fn upgrade_appends_the_block_when_there_is_none() {
        let dir = tempfile::tempdir().unwrap();
        project_with_agents(dir.path(), "Only my notes.\n");
        let report = upgrade(dir.path(), &bundle_with_block("0.2.0")).unwrap();
        let after = std::fs::read_to_string(dir.path().join("AGENTS.md")).unwrap();
        assert!(after.starts_with("Only my notes.\n"));
        assert!(after.contains("# myapp on @boogy/web 0.2.0"));
        assert_eq!(report.from, None);
    }

    #[test]
    fn upgrade_refuses_a_directory_that_is_not_a_project() {
        let dir = tempfile::tempdir().unwrap();
        assert!(upgrade(dir.path(), &bundle_with_block("0.2.0")).is_err());
    }
}
