use std::fs;

use anyhow::{Result, anyhow};

use crate::tool::Tool;

const TOOL: Tool = Tool::Codex;

pub fn switch(profile: &str) -> Result<()> {
    let profile_dir = TOOL.profile_dir(profile)?;
    if !profile_dir.exists() {
        return Err(anyhow!("profile '{}' does not exist for {}", profile, TOOL));
    }

    // Update _current file
    fs::write(TOOL.current_file()?, format!("{}\n", profile))?;

    // Sync auth.json to root
    let src = profile_dir.join("auth.json");
    if src.exists() {
        let dest = TOOL.home_dir()?.join("auth.json");
        fs::copy(&src, &dest)?;
    }

    Ok(())
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
