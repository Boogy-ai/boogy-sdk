use std::path::PathBuf;

#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct Credentials {
    pub token: String,
    pub handle: String,
    pub expires: u64,
}

/// Returns the path to the credentials file, honoring `$XDG_CONFIG_HOME` if
/// set, then falling back to the platform config dir (`~/.config` on Linux).
pub fn credentials_path() -> PathBuf {
    let base = if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
        PathBuf::from(xdg)
    } else {
        dirs::config_dir().unwrap_or_else(|| {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".config")
        })
    };
    base.join("boogy").join("credentials.toml")
}

/// Persist credentials to disk and restrict permissions to owner-only (unix).
///
/// On unix: the file is opened with mode 0600 from the start so there is no
/// window where a world-readable file exists before the chmod. A second
/// `set_permissions` call follows the write to fix any pre-existing file that
/// may have been created with looser permissions.
pub fn save(c: &Credentials) -> anyhow::Result<()> {
    let path = credentials_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let contents = toml::to_string(c)?;
    #[cfg(unix)]
    {
        use std::io::Write as _;
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&path)?;
        file.write_all(contents.as_bytes())?;
        // Also fix any pre-existing file that may have been created with looser
        // permissions by a previous version of save().
        let perms = std::fs::Permissions::from_mode(0o600);
        std::fs::set_permissions(&path, perms)?;
    }
    #[cfg(not(unix))]
    {
        std::fs::write(&path, &contents)?;
    }
    Ok(())
}

/// Load credentials from disk, returning `None` if the file is absent or
/// cannot be parsed.
pub fn load() -> Option<Credentials> {
    let path = credentials_path();
    let text = std::fs::read_to_string(&path).ok()?;
    toml::from_str(&text).ok()
}

/// Resolve the bearer token using precedence: explicit `flag` → `BOOGY_TOKEN`
/// env var → stored credentials file.
pub fn resolve_token(flag: Option<&str>) -> Option<String> {
    if let Some(t) = flag {
        return Some(t.to_owned());
    }
    if let Ok(t) = std::env::var("BOOGY_TOKEN") {
        if !t.is_empty() {
            return Some(t);
        }
    }
    load().map(|c| c.token)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_then_load_roundtrips_and_is_0600() {
        let dir = tempfile::tempdir().unwrap();
        // Safety: single-threaded test; no other test in this module touches XDG_CONFIG_HOME.
        unsafe {
            std::env::set_var("XDG_CONFIG_HOME", dir.path());
        }
        let c = Credentials {
            token: "v4.public.x".into(),
            handle: "swift-otter".into(),
            expires: 123,
        };
        save(&c).unwrap();
        let got = load().unwrap();
        assert_eq!(got.token, "v4.public.x");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(credentials_path())
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }
}
