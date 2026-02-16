use std::fs;

use anyhow::{Result, anyhow};

use super::keychain;
use crate::tool::Tool;

const TOOL: Tool = Tool::Claude;

pub fn switch(profile: &str) -> Result<()> {
    let profile_dir = TOOL.profile_dir(profile)?;
    if !profile_dir.exists() {
        return Err(anyhow!("profile '{}' does not exist for {}", profile, TOOL));
    }

    // Save current keychain to current profile's credentials.json
    sync_keychain_to_current_profile();

    // Load profile credentials into keychain
    let creds_path = profile_dir.join("credentials.json");
    if creds_path.exists() {
        let content = fs::read_to_string(&creds_path)?;
        let value: serde_json::Value = serde_json::from_str(&content)?;
        keychain::write(&value)?;
    }

    fs::write(TOOL.current_file()?, format!("{}\n", profile))?;
    Ok(())
}

fn sync_keychain_to_current_profile() {
    let current = match TOOL.current_profile() {
        Ok(Some(name)) => name,
        _ => return,
    };
    let creds_path = match TOOL.profile_dir(&current) {
        Ok(dir) => dir.join("credentials.json"),
        _ => return,
    };
    if !creds_path.exists() {
        return;
    }
    let value = match keychain::read() {
        Ok(v) => v,
        _ => return,
    };
    let json = match serde_json::to_string_pretty(&value) {
        Ok(j) => j,
        _ => return,
    };
    let _ = fs::write(&creds_path, json);
}

pub fn save(name: &str) -> Result<()> {
    let dest_dir = TOOL.profile_dir(name)?;
    if dest_dir.exists() {
        return Err(anyhow!("profile '{}' already exists for {}", name, TOOL));
    }

    // Read current credentials from keychain
    let creds = keychain::read()?;
    let json = serde_json::to_string_pretty(&creds)?;

    fs::create_dir_all(&dest_dir)?;
    fs::write(dest_dir.join("credentials.json"), json)?;
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
