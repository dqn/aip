use std::io::Write;
use std::process::{Command, Stdio};

use anyhow::{Result, anyhow};
use serde_json::Value;

const SERVICE: &str = "Claude Code-credentials";

fn account() -> Result<String> {
    std::env::var("USER")
        .or_else(|_| std::env::var("LOGNAME"))
        .map_err(|_| anyhow!("could not determine current user"))
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

    // Pass password via stdin to avoid exposure in process list
    let mut child = Command::new("security")
        .args(["add-generic-password", "-s", SERVICE, "-a", &acct, "-w"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;

    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(json_str.as_bytes())?;
        stdin.write_all(b"\n")?;
    }

    let output = child.wait_with_output()?;

    if !output.status.success() {
        return Err(anyhow!(
            "failed to write credentials to keychain: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }

    Ok(())
}
