use std::fs;

use anyhow::{Result, anyhow};

use crate::tool::Tool;

const TOOL: Tool = Tool::Codex;

pub fn switch(profile: &str) -> Result<()> {
    let profile_dir = TOOL.profile_dir(profile)?;
    if !profile_dir.exists() {
        return Err(anyhow!("profile '{}' does not exist for {}", profile, TOOL));
    }

    // Save active auth.json to current profile
    sync_auth_to_current_profile();

    // Record session cutoff for the outgoing profile
    if let Ok(Some(current)) = TOOL.current_profile()
        && current != profile
    {
        let cutoff = TOOL.profile_dir(&current)?.join("_session_cutoff");
        fs::write(&cutoff, "")?;
    }

    // Update _current file
    fs::write(TOOL.current_file()?, format!("{}\n", profile))?;

    // Load new profile's auth.json to root
    let src = profile_dir.join("auth.json");
    if src.exists() {
        let dest = TOOL.home_dir()?.join("auth.json");
        fs::copy(&src, &dest)?;
    }

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
    if src.exists() && dest.exists() {
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
            eprintln!(
                "Re-authenticate and run 'aip save' to save to the correct profile."
            );
            return;
        }

        if let Err(e) = fs::copy(&src, &dest) {
            eprintln!(
                "Warning: failed to sync auth to profile '{}': {}",
                current, e
            );
        }
    }
}

pub fn save(name: &str) -> Result<()> {
    let src = TOOL.home_dir()?.join("auth.json");
    if !src.exists() {
        return Err(anyhow!("auth.json not found in {}", TOOL));
    }

    let dest_dir = TOOL.profile_dir(name)?;
    if dest_dir.exists() {
        return Err(anyhow!("profile '{}' already exists for {}", name, TOOL));
    }

    fs::create_dir_all(&dest_dir)?;
    fs::copy(&src, dest_dir.join("auth.json"))?;
    Ok(())
}

pub fn delete(name: &str) -> Result<()> {
    let current = TOOL.current_profile()?;
    if current.as_deref() == Some(name) {
        return Err(anyhow!("cannot delete the current profile '{}'", name));
    }

    let profile_dir = TOOL.profile_dir(name)?;
    if !profile_dir.exists() {
        return Err(anyhow!("profile '{}' does not exist for {}", name, TOOL));
    }

    fs::remove_dir_all(&profile_dir)?;
    Ok(())
}
