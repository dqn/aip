use std::ffi::OsString;
use std::fs;
use std::os::unix::{
    fs::{PermissionsExt, symlink},
    process::CommandExt,
};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};

use crate::fs_util;
use crate::tool::Tool;

const TOOL: Tool = Tool::Codex;
const IMPORTED_REFRESH: &str = "aip-imported-refresh.sha256";

fn refresh_fingerprint(contents: &[u8]) -> Result<Option<String>> {
    let raw: serde_json::Value = serde_json::from_slice(contents)?;
    Ok(raw
        .get("tokens")
        .and_then(|tokens| tokens.get("refresh_token"))
        .and_then(|token| token.as_str())
        .filter(|token| !token.is_empty())
        .map(|token| {
            ring::digest::digest(&ring::digest::SHA256, token.as_bytes())
                .as_ref()
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect()
        }))
}

fn record_import(contents: &[u8], dir: &Path) -> Result<()> {
    let fingerprint = refresh_fingerprint(contents)?;
    let marker = dir.join(IMPORTED_REFRESH);
    let mut recorded = match fs::read_to_string(&marker) {
        Ok(recorded) => recorded,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e.into()),
    };
    if let Some(fingerprint) = fingerprint
        && !recorded.lines().any(|line| line == fingerprint)
    {
        recorded.push_str(&format!("{fingerprint}\n"));
    }
    // Retain prior imports too, so a failed replacement cannot enable an old copy.
    fs_util::atomic_write(&marker, &recorded)
}

fn check_refresh_ownership(dir: &Path, args: &[OsString]) -> Result<()> {
    let contents = fs::read(dir.join("auth.json"))?;
    let marker = dir.join(IMPORTED_REFRESH);
    if !marker.exists() {
        // Legacy snapshots also need an independent OAuth grant before use.
        record_import(&contents, dir)?;
    }
    if args.first().is_some_and(|arg| arg == "login") {
        return Ok(());
    }
    if let Some(current) = refresh_fingerprint(&contents)?
        && fs::read_to_string(marker)?
            .lines()
            .any(|line| line == current)
    {
        bail!(
            "imported OAuth credentials need an independent login; run 'aip codex login' once for this profile to keep the source session's token refresh separate"
        );
    }
    Ok(())
}

fn import_snapshot(contents: &str, dir: &Path) -> Result<()> {
    let dest = dir.join("auth.json");
    if dest.is_file() && !dir.join(IMPORTED_REFRESH).exists() {
        record_import(&fs::read(&dest)?, dir)?;
    }
    // Fingerprint and credentials must use one read, even if the source refreshes.
    record_import(contents.as_bytes(), dir)?;
    fs_util::atomic_write(&dest, contents)?;
    fs::set_permissions(&dest, fs::Permissions::from_mode(0o600))?;
    Ok(())
}

fn profile_dir(home: &Path, name: &str) -> Result<PathBuf> {
    TOOL.profile_dir(name)?;
    Ok(home.join("profiles").join(name))
}

pub fn switch(profile: &str) -> Result<()> {
    switch_at(&TOOL.home_dir()?, profile)
}

fn switch_at(home: &Path, profile: &str) -> Result<()> {
    let dir = profile_dir(home, profile)?;
    if !dir.join("auth.json").is_file() {
        bail!("credentials file not found for Codex profile '{profile}'");
    }
    // Selection only affects the next launch. Running homes keep their auth.
    fs_util::atomic_write(&home.join("profiles/_current"), &format!("{profile}\n"))
}

pub fn launch(args: &[OsString]) -> Result<()> {
    let mut command = launch_command_at(&TOOL.home_dir()?, args, Path::new("codex"))?;
    // Replace aip so terminal signals and the native exit status reach the caller.
    Err(command.exec()).context("could not launch Codex")
}

fn launch_command_at(home: &Path, args: &[OsString], codex: &Path) -> Result<Command> {
    let selected = fs::read_to_string(home.join("profiles/_current"))
        .context("no Codex profile selected; select one with aip or run 'aip save codex <name>'")?;
    let dir = profile_dir(home, selected.trim())?;
    if !dir.join("auth.json").is_file() {
        bail!("credentials file not found for selected Codex profile");
    }
    let dir = dir.canonicalize()?;
    check_refresh_ownership(&dir, args)?;
    share_settings(home, &dir)?;
    let mut command = Command::new(codex);
    command
        .env("CODEX_HOME", &dir)
        .env("CODEX_SQLITE_HOME", &dir)
        .args([
            "--config",
            "cli_auth_credentials_store=\"file\"",
            "--config",
        ])
        .arg(format!("sqlite_home={}", serde_json::to_string(&dir)?))
        .args(args)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    Ok(command)
}

fn share_settings(home: &Path, dir: &Path) -> Result<()> {
    // Share common settings/resources; Codex auth, server and session data stay private.
    const SHARED: &[&str] = &[
        "config.toml",
        "AGENTS.md",
        "AGENTS.override.md",
        "requirements.toml",
        "managed_config.toml",
        "skills",
        "rules",
        "plugins",
        "hooks",
        "hooks.json",
    ];
    for entry in fs::read_dir(home)? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name_str) = name.to_str() else {
            continue;
        };
        if !SHARED.contains(&name_str) && !name_str.ends_with(".config.toml") {
            continue;
        }
        let dest = dir.join(&name);
        match fs::symlink_metadata(&dest) {
            Ok(_) => continue, // Preserve profile-specific settings and existing links.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        symlink(home.canonicalize()?.join(&name), &dest)?;
    }
    Ok(())
}

pub fn ensure_stopped(dir: &Path) -> Result<()> {
    if dir
        .join("app-server-control/app-server-control.sock")
        .try_exists()?
    {
        bail!(
            "Codex profile has a server control socket; stop that profile's server before deleting or replacing its credentials"
        );
    }
    Ok(())
}

pub fn save(name: &str) -> Result<()> {
    let home = TOOL.home_dir()?;
    let source_home = std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.clone());
    save_at(&home, &source_home, name)
}

fn save_at(home: &Path, source_home: &Path, name: &str) -> Result<()> {
    let src = source_home.join("auth.json");
    if !src.is_file() {
        bail!("auth.json not found in Codex credential home");
    }
    let dest_dir = profile_dir(home, name)?;
    let dest = dest_dir.join("auth.json");
    let same_file = dest.exists() && src.canonicalize()? == dest.canonicalize()?;
    if !same_file {
        ensure_stopped(&dest_dir)?;
    }
    let newly_created = !dest_dir.exists();
    fs::create_dir_all(&dest_dir)?;
    let result = (|| -> Result<()> {
        if !same_file {
            import_snapshot(&fs::read_to_string(&src)?, &dest_dir)?;
        }
        switch_at(home, name)
    })();
    if result.is_err() && newly_created {
        let _ = fs::remove_dir_all(&dest_dir);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;

    fn api_auth(key: &str) -> String {
        serde_json::json!({"auth_mode": "apikey", "OPENAI_API_KEY": key}).to_string()
    }

    fn fixture() -> tempfile::TempDir {
        let home = tempfile::tempdir_in("/tmp").unwrap();
        for name in ["old", "new"] {
            fs::create_dir_all(home.path().join("profiles").join(name)).unwrap();
            fs::write(
                home.path().join("profiles").join(name).join("auth.json"),
                api_auth(&format!("{name}-sentinel")),
            )
            .unwrap();
        }
        fs::write(home.path().join("auth.json"), api_auth("global-sentinel")).unwrap();
        fs::write(home.path().join("profiles/_current"), "old\n").unwrap();
        home
    }

    #[test]
    fn selection_leaves_global_auth_and_existing_profile_unchanged() {
        let home = fixture();
        switch_at(home.path(), "new").unwrap();
        assert_eq!(
            fs::read_to_string(home.path().join("auth.json")).unwrap(),
            api_auth("global-sentinel")
        );
        assert_eq!(
            fs::read_to_string(home.path().join("profiles/old/auth.json")).unwrap(),
            api_auth("old-sentinel")
        );
        assert_eq!(
            fs::read_to_string(home.path().join("profiles/_current")).unwrap(),
            "new\n"
        );
    }

    #[test]
    fn invalid_selection_preserves_marker() {
        let home = fixture();
        for name in ["../old", "_current", "missing"] {
            assert!(switch_at(home.path(), name).is_err());
            assert_eq!(
                fs::read_to_string(home.path().join("profiles/_current")).unwrap(),
                "old\n"
            );
        }
    }

    #[test]
    fn launch_uses_canonical_home_and_preserves_settings_and_credentials() {
        let home = fixture();
        fs::write(
            home.path().join("config.toml"),
            "model = \"shared-model\"\n",
        )
        .unwrap();
        fs::create_dir(home.path().join("skills")).unwrap();
        fs::create_dir(home.path().join("sessions")).unwrap();
        fs::write(home.path().join("ci.config.toml"), "model = \"ci-model\"\n").unwrap();
        fs::write(
            home.path().join("profiles/new/AGENTS.md"),
            "Profile instructions",
        )
        .unwrap();
        switch_at(home.path(), "new").unwrap();
        let args = vec![OsString::from("resume"), OsString::from("--last")];
        let command =
            launch_command_at(home.path(), &args, Path::new("/nonexistent/codex")).unwrap();
        let selected = home.path().join("profiles/new").canonicalize().unwrap();
        assert!(
            command
                .get_envs()
                .any(|(k, v)| k == "CODEX_HOME" && v == Some(selected.as_os_str()))
        );
        assert_eq!(command.get_args().collect::<Vec<_>>()[4..], args);
        assert_eq!(
            fs::read_to_string(selected.join("auth.json")).unwrap(),
            api_auth("new-sentinel")
        );
        assert_eq!(
            fs::read_to_string(selected.join("AGENTS.md")).unwrap(),
            "Profile instructions"
        );
        assert_eq!(
            fs::read_to_string(selected.join("config.toml")).unwrap(),
            "model = \"shared-model\"\n"
        );
        assert!(selected.join("skills").is_dir());
        assert!(selected.join("ci.config.toml").is_file());
        assert!(!selected.join("sessions").exists());
        // Relaunching is idempotent and never restores an older auth snapshot.
        fs::write(
            selected.join("auth.json"),
            api_auth("native-refreshed-sentinel"),
        )
        .unwrap();
        launch_command_at(home.path(), &[], Path::new("/nonexistent/codex")).unwrap();
        assert_eq!(
            fs::read_to_string(selected.join("auth.json")).unwrap(),
            api_auth("native-refreshed-sentinel")
        );
    }

    #[test]
    fn no_selection_or_missing_auth_cannot_launch() {
        let home = tempfile::tempdir().unwrap();
        assert!(launch_command_at(home.path(), &[], Path::new("/nonexistent/codex")).is_err());
        fs::create_dir(home.path().join("profiles")).unwrap();
        fs::write(home.path().join("profiles/_current"), "missing").unwrap();
        assert!(launch_command_at(home.path(), &[], Path::new("/nonexistent/codex")).is_err());
    }

    #[test]
    fn imported_oauth_requires_independent_login_before_launch() {
        let home = fixture();
        let auth = home.path().join("profiles/old/auth.json");
        let original = r#"{"tokens":{"refresh_token":"imported-refresh"}}"#;
        fs::write(&auth, original).unwrap();
        assert!(launch_command_at(home.path(), &[], Path::new("/nonexistent/codex")).is_err());
        let login = [OsString::from("login")];
        launch_command_at(home.path(), &login, Path::new("/nonexistent/codex")).unwrap();
        assert_eq!(fs::read_to_string(&auth).unwrap(), original);
        fs::write(
            &auth,
            r#"{"tokens":{"refresh_token":"independent-refresh"}}"#,
        )
        .unwrap();
        launch_command_at(home.path(), &[], Path::new("/nonexistent/codex")).unwrap();
        fs::write(
            &auth,
            r#"{"tokens":{"refresh_token":"native-rotated-refresh"}}"#,
        )
        .unwrap();
        launch_command_at(home.path(), &[], Path::new("/nonexistent/codex")).unwrap();
        fs::write(&auth, original).unwrap();
        assert!(launch_command_at(home.path(), &[], Path::new("/nonexistent/codex")).is_err());
    }

    #[test]
    fn importing_oauth_again_requires_login_and_preserves_source() {
        let home = fixture();
        let auth = home.path().join("profiles/old/auth.json");
        // An API-key launch has already initialized this profile's import marker.
        launch_command_at(home.path(), &[], Path::new("/nonexistent/codex")).unwrap();
        let first = r#"{"tokens":{"refresh_token":"first-import"}}"#;
        let second = r#"{"tokens":{"refresh_token":"second-import"}}"#;
        fs::write(home.path().join("auth.json"), first).unwrap();
        save_at(home.path(), home.path(), "old").unwrap();
        assert!(launch_command_at(home.path(), &[], Path::new("/nonexistent/codex")).is_err());
        fs::write(home.path().join("auth.json"), second).unwrap();
        save_at(home.path(), home.path(), "old").unwrap();
        assert!(launch_command_at(home.path(), &[], Path::new("/nonexistent/codex")).is_err());
        fs::write(&auth, first).unwrap();
        assert!(launch_command_at(home.path(), &[], Path::new("/nonexistent/codex")).is_err());
        fs::write(&auth, r#"{"tokens":{"refresh_token":"new-login"}}"#).unwrap();
        let marker = fs::read(home.path().join("profiles/old").join(IMPORTED_REFRESH)).unwrap();
        save_at(home.path(), &home.path().join("profiles/old"), "old").unwrap();
        assert_eq!(
            fs::read(home.path().join("profiles/old").join(IMPORTED_REFRESH)).unwrap(),
            marker
        );
        launch_command_at(home.path(), &[], Path::new("/nonexistent/codex")).unwrap();
        assert_eq!(
            fs::read_to_string(home.path().join("auth.json")).unwrap(),
            second
        );
    }

    #[test]
    fn failed_import_does_not_enable_legacy_oauth() {
        let home = fixture();
        let dir = home.path().join("profiles/old");
        let original = r#"{"tokens":{"refresh_token":"legacy-refresh"}}"#;
        fs::write(dir.join("auth.json"), original).unwrap();
        fs::write(
            home.path().join("auth.json"),
            r#"{"tokens":{"refresh_token":"new-import"}}"#,
        )
        .unwrap();
        fs::create_dir(dir.join("auth.tmp")).unwrap();
        assert!(save_at(home.path(), home.path(), "old").is_err());
        assert_eq!(fs::read_to_string(dir.join("auth.json")).unwrap(), original);
        assert!(launch_command_at(home.path(), &[], Path::new("/nonexistent/codex")).is_err());
    }

    #[test]
    fn source_rotation_during_import_keeps_snapshot_blocked() {
        let home = fixture();
        let source = home.path().join("auth.json");
        let imported = r#"{"tokens":{"refresh_token":"before-source-rotation"}}"#;
        let rotated = r#"{"tokens":{"refresh_token":"after-source-rotation"}}"#;
        fs::write(&source, imported).unwrap();
        let snapshot = fs::read_to_string(&source).unwrap();
        fs::write(&source, rotated).unwrap();
        import_snapshot(&snapshot, &home.path().join("profiles/old")).unwrap();
        assert_eq!(
            fs::read_to_string(home.path().join("profiles/old/auth.json")).unwrap(),
            imported
        );
        assert!(launch_command_at(home.path(), &[], Path::new("/nonexistent/codex")).is_err());
        assert_eq!(fs::read_to_string(source).unwrap(), rotated);
    }

    #[test]
    fn save_imports_explicit_home_and_self_save_keeps_refreshed_auth() {
        let home = fixture();
        let source = tempfile::tempdir().unwrap();
        fs::write(
            source.path().join("auth.json"),
            api_auth("external-sentinel"),
        )
        .unwrap();
        save_at(home.path(), source.path(), "imported").unwrap();
        assert_eq!(
            fs::read_to_string(home.path().join("profiles/imported/auth.json")).unwrap(),
            api_auth("external-sentinel")
        );
        assert_eq!(
            fs::metadata(home.path().join("profiles/imported/auth.json"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        let native = home.path().join("profiles/old");
        fs::write(native.join("auth.json"), api_auth("fresh-sentinel")).unwrap();
        save_at(home.path(), &native, "old").unwrap();
        assert_eq!(
            fs::read_to_string(native.join("auth.json")).unwrap(),
            api_auth("fresh-sentinel")
        );
        assert_eq!(
            fs::read_to_string(home.path().join("auth.json")).unwrap(),
            api_auth("global-sentinel")
        );
    }

    #[test]
    fn running_profile_cannot_be_replaced() {
        let home = fixture();
        let target = home.path().join("profiles/old");
        fs::create_dir(target.join("app-server-control")).unwrap();
        let socket = target.join("app-server-control/app-server-control.sock");
        let _server = UnixListener::bind(socket).unwrap();
        assert!(ensure_stopped(&target).is_err());
        assert!(save_at(home.path(), home.path(), "old").is_err());
        assert_eq!(
            fs::read_to_string(target.join("auth.json")).unwrap(),
            api_auth("old-sentinel")
        );
        switch_at(home.path(), "new").unwrap();
        assert!(ensure_stopped(&target).is_err());
        save_at(home.path(), &target, "old").unwrap();
    }
}
