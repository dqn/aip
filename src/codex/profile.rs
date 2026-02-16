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
        let src_bytes = match fs::read(&src) {
            Ok(b) => b,
            _ => return,
        };
        let dest_bytes = match fs::read(&dest) {
            Ok(b) => b,
            _ => return,
        };
        if src_bytes != dest_bytes {
            eprintln!(
                "Warning: Current auth.json differs from profile '{}'.",
                current,
            );
            eprintln!("Skipping sync to protect stored credentials.");
            eprintln!("Run 'aip login' to re-authenticate and save to the correct profile.");
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

pub fn update(name: &str) -> Result<()> {
    let src = TOOL.home_dir()?.join("auth.json");
    if !src.exists() {
        return Err(anyhow!("auth.json not found in {}", TOOL));
    }

    let dest_dir = TOOL.profile_dir(name)?;
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
