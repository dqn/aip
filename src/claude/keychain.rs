use std::process::Command;

use anyhow::{Result, anyhow};
use serde_json::Value;

const SERVICE: &str = "Claude Code-credentials";

fn account() -> Result<String> {
    let output = Command::new("whoami").output()?;
    Ok(String::from_utf8(output.stdout)?.trim().to_string())
}

pub fn read() -> Result<Value> {
    let acct = account()?;
    let output = Command::new("security")
        .args(["find-generic-password", "-s", SERVICE, "-a", &acct, "-w"])
        .output()?;

    if !output.status.success() {
        return Err(anyhow!("no credentials found in keychain"));
    }

    let json_str = String::from_utf8(output.stdout)?;
    Ok(serde_json::from_str(json_str.trim())?)
}

pub fn write(value: &Value) -> Result<()> {
    let acct = account()?;
    let json_str = serde_json::to_string(value)?;

    // Delete existing entry (ignore errors if not found)
    let _ = Command::new("security")
        .args(["delete-generic-password", "-s", SERVICE, "-a", &acct])
        .output();

    let output = Command::new("security")
        .args([
            "add-generic-password",
            "-s",
            SERVICE,
            "-a",
            &acct,
            "-w",
            &json_str,
        ])
        .output()?;

    if !output.status.success() {
        return Err(anyhow!(
            "failed to write credentials to keychain: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }

    Ok(())
}
