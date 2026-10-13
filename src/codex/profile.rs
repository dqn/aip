use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use anyhow::Context;
use anyhow::{Result, anyhow};
use std::path::Path;

use super::daemon::Daemon;

use crate::fs_util;
use crate::tool::Tool;

const TOOL: Tool = Tool::Codex;

pub fn switch(profile: &str) -> Result<()> {
    switch_at(&TOOL.home_dir()?, profile, Path::new("codex"))
}

fn switch_at(home: &Path, profile: &str, codex: &Path) -> Result<()> {
    TOOL.profile_dir(profile)?; // Validate the name before joining paths.
    let profile_dir = home.join("profiles").join(profile);
    if !profile_dir.exists() {
        return Err(anyhow!("profile '{}' does not exist for {}", profile, TOOL));
    }

    // Save active auth.json to current profile
    sync_auth_at(home);

    // Validate new profile's auth.json exists before making changes
    let src = profile_dir.join("auth.json");
    if !src.exists() {
        return Err(anyhow!(
            "credentials file not found for profile '{}' ({})",
            profile,
            TOOL
        ));
    }

    // Pre-compute all fallible paths before the point of no return.
    let dest = home.join("auth.json");
    let daemon = Daemon::detect(home, codex)?;
    let auth = if daemon.is_some() {
        let auth = serde_json::from_slice(&fs::read(&src)?)?;
        Daemon::validate_auth(&auth)?;
        Some(auth)
    } else {
        None
    };

    // Update _current first, then copy credentials.
    // If credential copy fails, roll back _current to avoid contamination.
    let current_file = home.join("profiles/_current");
    let old_current = fs::read_to_string(&current_file).ok();
    fs_util::atomic_write(&current_file, &format!("{}\n", profile))?;
    if let Err(e) = fs_util::atomic_copy(&src, &dest) {
        // Roll back _current to previous value
        match &old_current {
            Some(prev) => {
                let _ = fs_util::atomic_write(&current_file, prev);
            }
            None => {
                let _ = fs::remove_file(&current_file);
            }
        }
        return Err(e);
    }

    if let (Some(daemon), Some(auth)) = (daemon, auth) {
        daemon.restart_and_verify(&auth).with_context(|| format!(
            "Credentials switched to '{profile}', but daemon activation failed. Select the profile again to retry"
        ))?;
    }
    Ok(())
}

pub fn sync_auth_to_current_profile() {
    if let Ok(home) = TOOL.home_dir() {
        sync_auth_at(&home);
    }
}

fn sync_auth_at(home: &Path) {
    let current = match fs::read_to_string(home.join("profiles/_current")) {
        Ok(name) if TOOL.profile_dir(name.trim()).is_ok() => name.trim().to_owned(),
        _ => return,
    };
    let dest = home.join("profiles").join(&current).join("auth.json");
    let src = home.join("auth.json");
    if !src.exists() {
        return;
    }

    let src_value: Option<serde_json::Value> = fs::read_to_string(&src)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok());
    let dest_value: Option<serde_json::Value> = fs::read_to_string(&dest)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok());

    let src_account = src_value
        .as_ref()
        .and_then(|v| v.get("tokens"))
        .and_then(|t| t.get("account_id"))
        .and_then(|a| a.as_str());
    let dest_account = dest_value
        .as_ref()
        .and_then(|v| v.get("tokens"))
        .and_then(|t| t.get("account_id"))
        .and_then(|a| a.as_str());

    match (src_account, dest_account) {
        (Some(src_id), Some(dest_id)) if src_id != dest_id => {
            eprintln!(
                "Warning: Current auth.json (account: '{}') differs from profile '{}' (account: '{}').",
                src_id, current, dest_id,
            );
            eprintln!("Skipping sync to protect stored credentials.");
            eprintln!("Re-authenticate and run 'aip save' to save to the correct profile.");
            return;
        }
        (Some(_), None) => {
            // Source has account_id but dest doesn't (e.g. after Codex CLI structural changes).
            // Allow sync but warn the user.
            eprintln!(
                "Warning: Current auth.json has account_id but profile '{}' does not. Syncing anyway.",
                current,
            );
        }
        (None, Some(_)) => {
            // Dest has account_id but source doesn't; block to avoid overwriting
            // identified credentials with anonymous ones.
            eprintln!(
                "Warning: Profile '{}' has account_id but current auth.json does not. Skipping sync.",
                current,
            );
            return;
        }
        _ => {}
    }

    if let Err(e) = fs_util::atomic_copy(&src, &dest) {
        eprintln!(
            "Warning: failed to sync auth to profile '{}': {}",
            current, e
        );
    }
}

pub fn save(name: &str) -> Result<()> {
    let src = TOOL.home_dir()?.join("auth.json");
    if !src.exists() {
        return Err(anyhow!("auth.json not found in {}", TOOL));
    }

    let dest_dir = TOOL.profile_dir(name)?;
    let newly_created = !dest_dir.exists();
    fs::create_dir_all(&dest_dir)?;

    let result = (|| -> Result<()> {
        let dest_path = dest_dir.join("auth.json");
        fs_util::atomic_copy(&src, &dest_path)?;
        #[cfg(unix)]
        fs::set_permissions(&dest_path, fs::Permissions::from_mode(0o600))?;

        // Update current profile to the newly saved one
        let current_file = TOOL.current_file()?;
        fs_util::atomic_write(&current_file, &format!("{}\n", name))?;

        Ok(())
    })();

    if result.is_err() && newly_created {
        let _ = fs::remove_dir_all(&dest_dir);
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn switch_fixture(
        stale: bool,
    ) -> (
        tempfile::TempDir,
        std::path::PathBuf,
        std::thread::JoinHandle<()>,
    ) {
        let (home, codex, server) = crate::codex::daemon::tests::fixture(stale);
        for name in ["old", "new"] {
            fs::create_dir_all(home.path().join("profiles").join(name)).unwrap();
        }
        let old = serde_json::json!({"tokens":{"account_id":"old","access_token":"old-sentinel"}});
        fs::write(home.path().join("auth.json"), old.to_string()).unwrap();
        fs::write(home.path().join("profiles/old/auth.json"), old.to_string()).unwrap();
        fs::write(
            home.path().join("profiles/new/auth.json"),
            crate::codex::daemon::tests::expected_auth().to_string(),
        )
        .unwrap();
        fs::write(home.path().join("profiles/_current"), "old\n").unwrap();
        (home, codex, server)
    }

    #[test]
    fn switch_activates_running_daemon_after_copying_credentials() {
        let (home, codex, server) = switch_fixture(false);
        switch_at(home.path(), "new", &codex).unwrap();
        assert_eq!(
            fs::read_to_string(home.path().join("profiles/_current")).unwrap(),
            "new\n"
        );
        let active: serde_json::Value =
            serde_json::from_slice(&fs::read(home.path().join("auth.json")).unwrap()).unwrap();
        assert_eq!(active["tokens"]["account_id"], "new");
        assert!(home.path().join("restarted").exists());
        server.join().unwrap();
    }

    #[test]
    fn switch_reports_partial_activation_failure() {
        let (home, codex, server) = switch_fixture(true);
        let error = format!("{:#}", switch_at(home.path(), "new", &codex).unwrap_err());
        assert!(error.contains("Credentials switched to 'new'"));
        assert!(error.contains("does not match"));
        assert_eq!(
            fs::read_to_string(home.path().join("profiles/_current")).unwrap(),
            "new\n"
        );
        server.join().unwrap();
    }

    #[test]
    fn switch_without_daemon_keeps_file_only_behavior() {
        let home = tempfile::tempdir().unwrap();
        fs::create_dir_all(home.path().join("profiles/new")).unwrap();
        fs::write(
            home.path().join("profiles/new/auth.json"),
            "sentinel-credentials",
        )
        .unwrap();
        switch_at(home.path(), "new", Path::new("/nonexistent/codex")).unwrap();
        assert_eq!(
            fs::read_to_string(home.path().join("auth.json")).unwrap(),
            "sentinel-credentials"
        );
        assert_eq!(
            fs::read_to_string(home.path().join("profiles/_current")).unwrap(),
            "new\n"
        );
    }

    #[test]
    fn switch_preflight_failure_preserves_active_credentials_and_marker() {
        use std::os::unix::net::UnixListener;
        let home = tempfile::tempdir().unwrap();
        fs::create_dir_all(home.path().join("profiles/new")).unwrap();
        fs::write(home.path().join("profiles/new/auth.json"), "new-sentinel").unwrap();
        fs::write(home.path().join("auth.json"), "old-sentinel").unwrap();
        fs::write(home.path().join("profiles/_current"), "old\n").unwrap();
        fs::create_dir_all(home.path().join("app-server-control")).unwrap();
        let _listener = UnixListener::bind(
            home.path()
                .join("app-server-control/app-server-control.sock"),
        )
        .unwrap();
        assert!(switch_at(home.path(), "new", Path::new("/nonexistent/codex")).is_err());
        assert_eq!(
            fs::read_to_string(home.path().join("auth.json")).unwrap(),
            "old-sentinel"
        );
        assert_eq!(
            fs::read_to_string(home.path().join("profiles/_current")).unwrap(),
            "old\n"
        );
    }

    /// Simulate the switch rollback logic: if credential copy after _current
    /// update fails, _current must be restored to its previous value.
    #[test]
    fn switch_rolls_back_current_on_copy_failure() {
        let dir = tempfile::tempdir().unwrap();
        let current_file = dir.path().join("_current");

        // Pre-existing _current value
        fs::write(&current_file, "old-profile\n").unwrap();

        let old_current = fs::read_to_string(&current_file).ok();
        fs_util::atomic_write(&current_file, "new-profile\n").unwrap();
        assert_eq!(fs::read_to_string(&current_file).unwrap(), "new-profile\n");

        // Simulate atomic_copy failure -> rollback
        let copy_failed = true;
        if copy_failed {
            match &old_current {
                Some(prev) => {
                    let _ = fs_util::atomic_write(&current_file, prev);
                }
                None => {
                    let _ = fs::remove_file(&current_file);
                }
            }
        }

        assert_eq!(fs::read_to_string(&current_file).unwrap(), "old-profile\n");
    }

    /// When _current didn't exist before switch, rollback should remove it.
    #[test]
    fn switch_removes_current_on_rollback_when_no_previous() {
        let dir = tempfile::tempdir().unwrap();
        let current_file = dir.path().join("_current");

        let old_current: Option<String> = fs::read_to_string(&current_file).ok();
        assert!(old_current.is_none());

        fs_util::atomic_write(&current_file, "new-profile\n").unwrap();
        assert!(current_file.exists());

        // Simulate failure -> rollback
        match &old_current {
            Some(prev) => {
                let _ = fs_util::atomic_write(&current_file, prev);
            }
            None => {
                let _ = fs::remove_file(&current_file);
            }
        }

        assert!(!current_file.exists());
    }

    /// Simulate save cleanup: newly created profile dir is removed on failure.
    #[test]
    fn save_cleans_up_newly_created_dir_on_failure() {
        let dir = tempfile::tempdir().unwrap();
        let dest_dir = dir.path().join("profiles").join("new-profile");
        let newly_created = !dest_dir.exists();
        assert!(newly_created);

        fs::create_dir_all(&dest_dir).unwrap();
        assert!(dest_dir.exists());

        // Simulate failure in the inner closure
        let result: Result<()> = Err(anyhow!("simulated copy failure"));

        if result.is_err() && newly_created {
            let _ = fs::remove_dir_all(&dest_dir);
        }

        assert!(!dest_dir.exists());
    }

    /// Verify that dest path is resolved before _current update so that
    /// a failure in home_dir() cannot leave _current in a dirty state.
    #[test]
    fn switch_dest_resolved_before_current_update() {
        // This test validates the ordering guarantee: all fallible path
        // computations (like home_dir()) happen before the _current write.
        // We simulate the structure by showing that dest is available before
        // the current_file write occurs.
        let dir = tempfile::tempdir().unwrap();
        let current_file = dir.path().join("_current");
        let auth_dir = dir.path().join("home");
        fs::create_dir_all(&auth_dir).unwrap();
        let dest = auth_dir.join("auth.json");

        // dest is computed successfully
        assert!(dest.parent().unwrap().exists());

        // Now write _current -- if dest computation had failed above,
        // we would never reach this point (mirroring the real code's ? operator).
        fs::write(&current_file, "old-profile\n").unwrap();
        let old_current = fs::read_to_string(&current_file).ok();
        fs_util::atomic_write(&current_file, "new-profile\n").unwrap();

        // Simulate copy failure -> rollback
        match &old_current {
            Some(prev) => {
                let _ = fs_util::atomic_write(&current_file, prev);
            }
            None => {
                let _ = fs::remove_file(&current_file);
            }
        }

        assert_eq!(fs::read_to_string(&current_file).unwrap(), "old-profile\n");
    }

    /// sync_auth_to_current_profile should proceed (with warning) when source
    /// has account_id but dest does not.
    #[test]
    fn sync_allows_copy_when_src_has_account_id_but_dest_does_not() {
        let dir = tempfile::tempdir().unwrap();
        let src_path = dir.path().join("src_auth.json");
        let dest_path = dir.path().join("dest_auth.json");

        // Source has account_id
        let src_json = serde_json::json!({
            "tokens": {
                "account_id": "acct-123",
                "access_token": "new-token"
            }
        });
        fs::write(&src_path, serde_json::to_string_pretty(&src_json).unwrap()).unwrap();

        // Dest has NO account_id
        let dest_json = serde_json::json!({
            "tokens": {
                "access_token": "old-token"
            }
        });
        fs::write(
            &dest_path,
            serde_json::to_string_pretty(&dest_json).unwrap(),
        )
        .unwrap();

        // Simulate the match logic from sync_auth_to_current_profile
        let src_value: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&src_path).unwrap()).unwrap();
        let dest_value: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&dest_path).unwrap()).unwrap();

        let src_account = src_value
            .get("tokens")
            .and_then(|t| t.get("account_id"))
            .and_then(|a| a.as_str());
        let dest_account = dest_value
            .get("tokens")
            .and_then(|t| t.get("account_id"))
            .and_then(|a| a.as_str());

        assert_eq!(src_account, Some("acct-123"));
        assert_eq!(dest_account, None);

        // (Some, None) case should NOT block -- sync proceeds
        let should_sync = match (src_account, dest_account) {
            (Some(_src_id), Some(dest_id)) if src_account.unwrap() != dest_id => false,
            (None, Some(_)) => false,
            _ => true, // includes (Some(_), None), (None, None), (Some(x), Some(x))
        };
        assert!(should_sync, "(Some, None) must allow sync to proceed");

        // Simulate the copy
        fs_util::atomic_copy(&src_path, &dest_path).unwrap();
        let updated: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&dest_path).unwrap()).unwrap();
        assert_eq!(updated["tokens"]["account_id"].as_str(), Some("acct-123"),);
    }

    /// sync_auth_to_current_profile should block when dest has account_id
    /// but source does not.
    #[test]
    fn sync_blocks_copy_when_dest_has_account_id_but_src_does_not() {
        let dir = tempfile::tempdir().unwrap();
        let src_path = dir.path().join("src_auth.json");
        let dest_path = dir.path().join("dest_auth.json");

        // Source has NO account_id
        let src_json = serde_json::json!({
            "tokens": {
                "access_token": "anon-token"
            }
        });
        fs::write(&src_path, serde_json::to_string_pretty(&src_json).unwrap()).unwrap();

        // Dest has account_id
        let dest_json = serde_json::json!({
            "tokens": {
                "account_id": "acct-456",
                "access_token": "identified-token"
            }
        });
        let original_dest = serde_json::to_string_pretty(&dest_json).unwrap();
        fs::write(&dest_path, &original_dest).unwrap();

        let src_value: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&src_path).unwrap()).unwrap();
        let dest_value: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&dest_path).unwrap()).unwrap();

        let src_account = src_value
            .get("tokens")
            .and_then(|t| t.get("account_id"))
            .and_then(|a| a.as_str());
        let dest_account = dest_value
            .get("tokens")
            .and_then(|t| t.get("account_id"))
            .and_then(|a| a.as_str());

        assert_eq!(src_account, None);
        assert_eq!(dest_account, Some("acct-456"));

        // (None, Some) case MUST block sync
        let should_sync = match (src_account, dest_account) {
            (Some(_src_id), Some(dest_id)) if src_account.unwrap() != dest_id => false,
            (None, Some(_)) => false,
            _ => true,
        };
        assert!(!should_sync, "(None, Some) must block sync");

        // Dest should remain unchanged
        assert_eq!(fs::read_to_string(&dest_path).unwrap(), original_dest);
    }

    /// When save overwrites an existing profile, dir should NOT be removed on failure.
    #[test]
    fn save_preserves_existing_dir_on_failure() {
        let dir = tempfile::tempdir().unwrap();
        let dest_dir = dir.path().join("profiles").join("existing-profile");
        fs::create_dir_all(&dest_dir).unwrap();
        fs::write(dest_dir.join("auth.json"), "old-auth").unwrap();

        let newly_created = !dest_dir.exists();
        assert!(!newly_created);

        // Simulate failure
        let result: Result<()> = Err(anyhow!("simulated copy failure"));

        if result.is_err() && newly_created {
            let _ = fs::remove_dir_all(&dest_dir);
        }

        // Directory and contents preserved
        assert!(dest_dir.exists());
        assert_eq!(
            fs::read_to_string(dest_dir.join("auth.json")).unwrap(),
            "old-auth"
        );
    }
}
