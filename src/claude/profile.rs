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

    // Atomic write for _current
    let current_file = TOOL.current_file()?;
    let tmp = current_file.with_extension("tmp");
    fs::write(&tmp, format!("{}\n", profile))?;
    if let Err(e) = fs::rename(&tmp, &current_file) {
        let _ = fs::remove_file(&tmp);
        return Err(e.into());
    }
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
    let keychain_value = match keychain::read() {
        Ok(v) => v,
        _ => return,
    };

    // Compare organizationName to detect account mismatch
    let stored_value: serde_json::Value = match fs::read_to_string(&creds_path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
    {
        Some(v) => v,
        None => serde_json::Value::Null,
    };

    let keychain_org = keychain_value
        .get("claudeAiOauth")
        .and_then(|o| o.get("organizationName"))
        .and_then(|v| v.as_str());
    let stored_org = stored_value
        .get("claudeAiOauth")
        .and_then(|o| o.get("organizationName"))
        .and_then(|v| v.as_str());

    if let (Some(k_org), Some(s_org)) = (keychain_org, stored_org)
        && k_org != s_org
    {
        eprintln!(
            "Warning: Current credentials (org: '{}') differ from profile '{}' (org: '{}').",
            k_org, current, s_org,
        );
        eprintln!("Skipping sync to protect stored credentials.");
        eprintln!("Re-authenticate and run 'aip save' to save to the correct profile.");
        return;
    }

    let json = match serde_json::to_string_pretty(&keychain_value) {
        Ok(j) => j,
        _ => return,
    };
    let tmp = creds_path.with_extension("tmp");
    if let Err(e) = fs::write(&tmp, &json).and_then(|_| fs::rename(&tmp, &creds_path)) {
        let _ = fs::remove_file(&tmp);
        eprintln!(
            "Warning: failed to sync credentials to profile '{}': {}",
            current, e
        );
    }
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
    if let Err(e) = fs::write(dest_dir.join("credentials.json"), json) {
        let _ = fs::remove_dir_all(&dest_dir);
        return Err(e.into());
    }
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
