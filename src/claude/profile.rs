use std::fs;

use anyhow::{Result, anyhow};

use crate::tool::Tool;

const TOOL: Tool = Tool::Claude;

pub fn switch(profile: &str) -> Result<()> {
    let profile_dir = TOOL.profile_dir(profile)?;
    if !profile_dir.exists() {
        return Err(anyhow!("profile '{}' does not exist for {}", profile, TOOL));
    }
    fs::write(TOOL.current_file()?, format!("{}\n", profile))?;
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
    fs::copy(&src, dest_dir.join("credentials.json"))?;
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
