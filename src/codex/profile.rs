use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use anyhow::{Result, anyhow};

use crate::fs_util;
use crate::tool::Tool;

const TOOL: Tool = Tool::Codex;

pub fn switch(profile: &str) -> Result<()> {
    let profile_dir = TOOL.profile_dir(profile)?;
    if !profile_dir.exists() {
        return Err(anyhow!("profile '{}' does not exist for {}", profile, TOOL));
    }

    // Save active auth.json to current profile
    sync_auth_to_current_profile();

    // Load new profile's auth.json to root (atomic write before updating _current)
    let src = profile_dir.join("auth.json");
    if src.exists() {
        let dest = TOOL.home_dir()?.join("auth.json");
        fs_util::atomic_copy(&src, &dest)?;
    }

    // Update _current file (atomic write)
    let current_file = TOOL.current_file()?;
    fs_util::atomic_write(&current_file, &format!("{}\n", profile))?;

    Ok(())
}

fn sync_auth_to_current_profile() {
    let current = match TOOL.current_profile() {
        Ok(Some(name)) => name,
        _ => return,
    };
    let dest = match TOOL.profile_dir(&current) {
        Ok(dir) => dir.join("auth.json"),
        _ => return,
    };
    let src = match TOOL.home_dir() {
        Ok(dir) => dir.join("auth.json"),
        _ => return,
    };
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

    if let (Some(src_id), Some(dest_id)) = (src_account, dest_account)
        && src_id != dest_id
    {
        eprintln!(
            "Warning: Current auth.json (account: '{}') differs from profile '{}' (account: '{}').",
            src_id, current, dest_id,
        );
        eprintln!("Skipping sync to protect stored credentials.");
        eprintln!("Re-authenticate and run 'aip save' to save to the correct profile.");
        return;
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
    fs::create_dir_all(&dest_dir)?;
    let dest_path = dest_dir.join("auth.json");
    if let Err(e) = fs::copy(&src, &dest_path) {
        let _ = fs::remove_dir_all(&dest_dir);
        return Err(e.into());
    }
    #[cfg(unix)]
    fs::set_permissions(&dest_path, fs::Permissions::from_mode(0o600))?;

    // Update current profile to the newly saved one
    let current_file = TOOL.current_file()?;
    fs_util::atomic_write(&current_file, &format!("{}\n", name))?;

    Ok(())
}
