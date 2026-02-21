use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use anyhow::{Result, anyhow};

use crate::fs_util;
use crate::tool::Tool;

const TOOL: Tool = Tool::Claude;

pub fn switch(profile: &str) -> Result<()> {
    let profile_dir = TOOL.profile_dir(profile)?;
    if !profile_dir.exists() {
        return Err(anyhow!("profile '{}' does not exist for {}", profile, TOOL));
    }

    // Save active credentials.json to current profile
    sync_credentials_to_current_profile();

    // Load new profile's credentials.json to root
    let src = profile_dir.join("credentials.json");
    if src.exists() {
        let dest = TOOL.home_dir()?.join("credentials.json");
        fs_util::atomic_copy(&src, &dest)?;
    }

    // Update _current file
    let current_file = TOOL.current_file()?;
    fs_util::atomic_write(&current_file, &format!("{}\n", profile))?;
    Ok(())
}

fn sync_credentials_to_current_profile() {
    let current = match TOOL.current_profile() {
        Ok(Some(name)) => name,
        _ => return,
    };
    let dest = match TOOL.profile_dir(&current) {
        Ok(dir) => dir.join("credentials.json"),
        _ => return,
    };
    let src = match TOOL.home_dir() {
        Ok(dir) => dir.join("credentials.json"),
        _ => return,
    };
    if !src.exists() {
        return;
    }
    if let Err(e) = fs_util::atomic_copy(&src, &dest) {
        eprintln!(
            "Warning: failed to sync credentials to profile '{}': {}",
            current, e
        );
    }
}

pub fn save(name: &str) -> Result<()> {
    let src = TOOL.home_dir()?.join("credentials.json");
    if !src.exists() {
        return Err(anyhow!("credentials.json not found in {}", TOOL));
    }

    let dest_dir = TOOL.profile_dir(name)?;
    fs::create_dir_all(&dest_dir)?;
    let creds_path = dest_dir.join("credentials.json");
    if let Err(e) = fs::copy(&src, &creds_path) {
        let _ = fs::remove_dir_all(&dest_dir);
        return Err(e.into());
    }
    #[cfg(unix)]
    fs::set_permissions(&creds_path, fs::Permissions::from_mode(0o600))?;

    // Update current profile to the newly saved one
    let current_file = TOOL.current_file()?;
    fs_util::atomic_write(&current_file, &format!("{}\n", name))?;

    Ok(())
}
