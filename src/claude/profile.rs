use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::process::Command;

use anyhow::{Result, anyhow};

use crate::fs_util;
use crate::tool::Tool;

const TOOL: Tool = Tool::Claude;
const KEYCHAIN_SERVICE: &str = "Claude Code-credentials";

/// Decode hex-encoded credentials returned by `security -w` for blob entries.
///
/// Claude Code stores credentials as a binary blob in Keychain.
/// `security find-generic-password -w` returns blob data as a hex string
/// (e.g. "7b0a2022..." for '{\n "...'), which must be decoded back to JSON.
fn decode_hex_credentials(data: &str) -> String {
    if data.starts_with('{') {
        return data.to_string();
    }
    if !data.len().is_multiple_of(2) || !data.bytes().all(|b| b.is_ascii_hexdigit()) {
        return data.to_string();
    }
    let bytes: Vec<u8> = (0..data.len())
        .step_by(2)
        .filter_map(|i| u8::from_str_radix(&data[i..i + 2], 16).ok())
        .collect();
    match String::from_utf8(bytes) {
        Ok(s) if s.starts_with('{') => s,
        _ => data.to_string(),
    }
}

fn read_keychain() -> Result<String> {
    let output = Command::new("security")
        .args(["find-generic-password", "-s", KEYCHAIN_SERVICE, "-w"])
        .output()?;
    if !output.status.success() {
        return Err(anyhow!(
            "failed to read credentials from Keychain (service: {})",
            KEYCHAIN_SERVICE
        ));
    }
    let data = String::from_utf8(output.stdout)?;
    let trimmed = data.trim_end_matches('\n');
    if trimmed.is_empty() {
        return Err(anyhow!("Keychain entry is empty"));
    }
    Ok(decode_hex_credentials(trimmed))
}

fn write_keychain(data: &str) -> Result<()> {
    let account = std::env::var("USER").unwrap_or_else(|_| "unknown".to_string());

    // Delete existing entry (ignore errors if it doesn't exist)
    let _ = Command::new("security")
        .args(["delete-generic-password", "-s", KEYCHAIN_SERVICE])
        .output();

    let output = Command::new("security")
        .args([
            "add-generic-password",
            "-s",
            KEYCHAIN_SERVICE,
            "-a",
            &account,
            "-w",
            data,
        ])
        .output()?;
    if !output.status.success() {
        return Err(anyhow!(
            "failed to write credentials to Keychain (service: {})",
            KEYCHAIN_SERVICE
        ));
    }
    Ok(())
}

pub fn switch(profile: &str) -> Result<()> {
    let profile_dir = TOOL.profile_dir(profile)?;
    if !profile_dir.exists() {
        return Err(anyhow!("profile '{}' does not exist for {}", profile, TOOL));
    }

    // Save current Keychain credentials to current profile
    sync_keychain_to_current_profile();

    // Load new profile's credentials into Keychain
    let src = profile_dir.join("credentials.json");
    if src.exists() {
        let raw = fs::read_to_string(&src)?;
        let data = decode_hex_credentials(&raw);
        // Persist decoded credentials back to file if hex was decoded
        if data != raw {
            let _ = fs_util::atomic_write(&src, &data);
            #[cfg(unix)]
            let _ = fs::set_permissions(&src, fs::Permissions::from_mode(0o600));
        }
        write_keychain(&data)?;
    }

    // Update _current file
    let current_file = TOOL.current_file()?;
    fs_util::atomic_write(&current_file, &format!("{}\n", profile))?;
    Ok(())
}

pub fn sync_keychain_to_current_profile() {
    let current = match TOOL.current_profile() {
        Ok(Some(name)) => name,
        _ => return,
    };
    let dest = match TOOL.profile_dir(&current) {
        Ok(dir) => dir.join("credentials.json"),
        _ => return,
    };
    let data = match read_keychain() {
        Ok(d) => d,
        Err(_) => return,
    };
    if let Err(e) = fs_util::atomic_write(&dest, &data) {
        eprintln!(
            "Warning: failed to sync credentials to profile '{}': {}",
            current, e
        );
        return;
    }
    #[cfg(unix)]
    let _ = fs::set_permissions(&dest, fs::Permissions::from_mode(0o600));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_hex_credentials_passes_through_json() {
        let json = r#"{"claudeAiOauth":{"accessToken":"abc"}}"#;
        assert_eq!(decode_hex_credentials(json), json);
    }

    #[test]
    fn decode_hex_credentials_decodes_hex_encoded_json() {
        let json = r#"{"key":"value"}"#;
        let hex: String = json.bytes().map(|b| format!("{:02x}", b)).collect();
        assert_eq!(decode_hex_credentials(&hex), json);
    }

    #[test]
    fn decode_hex_credentials_passes_through_non_hex() {
        let data = "not-hex-data!@#";
        assert_eq!(decode_hex_credentials(data), data);
    }

    #[test]
    fn decode_hex_credentials_passes_through_odd_length_hex() {
        let data = "7b0";
        assert_eq!(decode_hex_credentials(data), data);
    }

    #[test]
    fn decode_hex_credentials_passes_through_hex_that_is_not_json() {
        // Hex that decodes to non-JSON
        let data = "48454c4c4f"; // "HELLO"
        assert_eq!(decode_hex_credentials(data), data);
    }
}

pub fn save(name: &str) -> Result<()> {
    let data = read_keychain()?;

    let dest_dir = TOOL.profile_dir(name)?;
    fs::create_dir_all(&dest_dir)?;
    let creds_path = dest_dir.join("credentials.json");
    fs::write(&creds_path, &data)?;
    #[cfg(unix)]
    fs::set_permissions(&creds_path, fs::Permissions::from_mode(0o600))?;

    // Update current profile to the newly saved one
    let current_file = TOOL.current_file()?;
    fs_util::atomic_write(&current_file, &format!("{}\n", name))?;

    Ok(())
}
