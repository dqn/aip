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

fn encode_hex(data: &str) -> String {
    data.bytes().map(|b| format!("{:02x}", b)).collect()
}

fn write_keychain(data: &str) -> Result<()> {
    let account = std::env::var("USER").unwrap_or_else(|_| "unknown".to_string());
    let hex_data = encode_hex(data);

    // Store as hex blob (-X) to match Claude Code's format.
    // Claude Code reads Keychain with `security -w` which returns hex for blob
    // entries, then hex-decodes before JSON parsing.
    // -U updates an existing entry or creates a new one atomically.
    let output = Command::new("security")
        .args([
            "add-generic-password",
            "-U",
            "-s",
            KEYCHAIN_SERVICE,
            "-a",
            &account,
            "-X",
            &hex_data,
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

    // Load new profile's credentials
    let src = profile_dir.join("credentials.json");
    if !src.exists() {
        return Err(anyhow!(
            "credentials file not found for profile '{}' ({})",
            profile,
            TOOL
        ));
    }
    let raw = fs::read_to_string(&src)?;
    let data = decode_hex_credentials(&raw);
    // Persist decoded credentials back to file if hex was decoded
    if data != raw {
        if let Err(e) = fs_util::atomic_write(&src, &data) {
            eprintln!("warning: failed to update credentials format: {e}");
        }
        #[cfg(unix)]
        if let Err(e) = fs::set_permissions(&src, fs::Permissions::from_mode(0o600)) {
            eprintln!("warning: failed to set credentials file permissions: {e}");
        }
    }

    // Update _current first, then write credentials to Keychain.
    // If Keychain write fails, roll back _current to avoid contamination.
    let current_file = TOOL.current_file()?;
    let old_current = fs::read_to_string(&current_file).ok();
    fs_util::atomic_write(&current_file, &format!("{}\n", profile))?;

    if let Err(e) = write_keychain(&data) {
        // Roll back _current to previous value
        match &old_current {
            Some(prev) => {
                let _ = fs_util::atomic_write(&current_file, prev);
            }
            None => {
                let _ = fs::remove_file(&current_file);
            }
        }
        return Err(e);
    }

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

    /// Simulate the switch rollback logic: if the operation after _current
    /// update fails, _current must be restored to its previous value.
    #[test]
    fn switch_rolls_back_current_on_credential_failure() {
        let dir = tempfile::tempdir().unwrap();
        let current_file = dir.path().join("_current");

        // Pre-existing _current value
        fs::write(&current_file, "old-profile\n").unwrap();

        let old_current = fs::read_to_string(&current_file).ok();
        fs_util::atomic_write(&current_file, "new-profile\n").unwrap();
        assert_eq!(fs::read_to_string(&current_file).unwrap(), "new-profile\n");

        // Simulate credential write failure -> rollback
        let credential_write_failed = true;
        if credential_write_failed {
            match &old_current {
                Some(prev) => {
                    let _ = fs_util::atomic_write(&current_file, prev);
                }
                None => {
                    let _ = fs::remove_file(&current_file);
                }
            }
        }

        assert_eq!(fs::read_to_string(&current_file).unwrap(), "old-profile\n");
    }

    /// When _current didn't exist before switch, rollback should remove it.
    #[test]
    fn switch_removes_current_on_rollback_when_no_previous() {
        let dir = tempfile::tempdir().unwrap();
        let current_file = dir.path().join("_current");

        let old_current: Option<String> = fs::read_to_string(&current_file).ok();
        assert!(old_current.is_none());

        fs_util::atomic_write(&current_file, "new-profile\n").unwrap();
        assert!(current_file.exists());

        // Simulate failure -> rollback
        match &old_current {
            Some(prev) => {
                let _ = fs_util::atomic_write(&current_file, prev);
            }
            None => {
                let _ = fs::remove_file(&current_file);
            }
        }

        assert!(!current_file.exists());
    }

    /// Simulate save cleanup: newly created profile dir is removed on failure.
    #[test]
    fn save_cleans_up_newly_created_dir_on_failure() {
        let dir = tempfile::tempdir().unwrap();
        let dest_dir = dir.path().join("profiles").join("new-profile");
        let newly_created = !dest_dir.exists();
        assert!(newly_created);

        fs::create_dir_all(&dest_dir).unwrap();
        assert!(dest_dir.exists());

        // Simulate failure in the inner closure
        let result: Result<()> = Err(anyhow!("simulated write failure"));

        if result.is_err() && newly_created {
            let _ = fs::remove_dir_all(&dest_dir);
        }

        assert!(!dest_dir.exists());
    }

    /// When save overwrites an existing profile, dir should NOT be removed on failure.
    #[test]
    fn save_preserves_existing_dir_on_failure() {
        let dir = tempfile::tempdir().unwrap();
        let dest_dir = dir.path().join("profiles").join("existing-profile");
        fs::create_dir_all(&dest_dir).unwrap();
        fs::write(dest_dir.join("credentials.json"), "old-creds").unwrap();

        let newly_created = !dest_dir.exists();
        assert!(!newly_created);

        // Simulate failure
        let result: Result<()> = Err(anyhow!("simulated write failure"));

        if result.is_err() && newly_created {
            let _ = fs::remove_dir_all(&dest_dir);
        }

        // Directory and contents preserved
        assert!(dest_dir.exists());
        assert_eq!(
            fs::read_to_string(dest_dir.join("credentials.json")).unwrap(),
            "old-creds"
        );
    }

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

    #[test]
    fn encode_hex_round_trips_with_decode() {
        let json = r#"{"claudeAiOauth":{"accessToken":"abc"}}"#;
        let hex = encode_hex(json);
        assert_eq!(decode_hex_credentials(&hex), json);
    }

    #[test]
    fn encode_hex_produces_lowercase_hex() {
        assert_eq!(encode_hex("AB"), "4142");
    }
}

pub fn save(name: &str) -> Result<()> {
    let data = read_keychain()?;

    let dest_dir = TOOL.profile_dir(name)?;
    let newly_created = !dest_dir.exists();
    fs::create_dir_all(&dest_dir)?;

    let result = (|| -> Result<()> {
        let creds_path = dest_dir.join("credentials.json");
        fs_util::atomic_write(&creds_path, &data)?;
        #[cfg(unix)]
        fs::set_permissions(&creds_path, fs::Permissions::from_mode(0o600))?;

        // Update current profile to the newly saved one
        let current_file = TOOL.current_file()?;
        fs_util::atomic_write(&current_file, &format!("{}\n", name))?;

        Ok(())
    })();

    if result.is_err() && newly_created {
        let _ = fs::remove_dir_all(&dest_dir);
    }

    result
}
