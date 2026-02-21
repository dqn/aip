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

    // Atomic write for _current
    let current_file = TOOL.current_file()?;
    fs_util::atomic_write(&current_file, &format!("{}\n", profile))?;
    Ok(())
}

pub fn save(name: &str) -> Result<()> {
    let current = TOOL
        .current_profile()?
        .ok_or_else(|| anyhow!("no current profile set for {}", TOOL))?;

    let src = TOOL.profile_dir(&current)?.join("credentials.json");
    if !src.exists() {
        return Err(anyhow!(
            "credentials.json not found in current profile '{}'",
            current
        ));
    }

    let dest_dir = TOOL.profile_dir(name)?;
    if dest_dir.exists() {
        return Err(anyhow!("profile '{}' already exists for {}", name, TOOL));
    }

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
