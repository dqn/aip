use std::collections::HashSet;
use std::fmt;
use std::path::PathBuf;

use anyhow::{Result, anyhow};

use crate::fs_util;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Tool {
    Claude,
    Codex,
}

impl Tool {
    pub const ALL: [Tool; 2] = [Tool::Claude, Tool::Codex];

    pub fn home_dir(&self) -> Result<PathBuf> {
        let home = dirs::home_dir().ok_or_else(|| anyhow!("could not determine home directory"))?;
        match self {
            Tool::Claude => Ok(home.join(".claude")),
            Tool::Codex => Ok(home.join(".codex")),
        }
    }

    pub fn profiles_dir(&self) -> Result<PathBuf> {
        Ok(self.home_dir()?.join("profiles"))
    }

    pub fn current_file(&self) -> Result<PathBuf> {
        Ok(self.profiles_dir()?.join("_current"))
    }

    pub fn current_profile(&self) -> Result<Option<String>> {
        let path = self.current_file()?;
        if !path.exists() {
            return Ok(None);
        }
        let content = std::fs::read_to_string(&path)?;
        let trimmed = content.trim();
        if trimmed.is_empty() {
            return Ok(None);
        }
        if self.profile_dir(trimmed).is_err() {
            return Ok(None);
        }
        Ok(Some(trimmed.to_string()))
    }

    pub fn profile_dir(&self, name: &str) -> Result<PathBuf> {
        if name.is_empty()
            || !name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            return Err(anyhow!(
                "invalid profile name: '{}' (only ASCII alphanumeric, '-', '_' allowed)",
                name
            ));
        }
        if name == "_current" || name == "_order" {
            return Err(anyhow!("'{}' is a reserved name", name));
        }
        Ok(self.profiles_dir()?.join(name))
    }

    pub fn delete_profile(&self, name: &str) -> Result<()> {
        let current = self.current_profile()?;
        if current.as_deref() == Some(name) {
            return Err(anyhow!("cannot delete the current profile '{}'", name));
        }

        let profile_dir = self.profile_dir(name)?;
        if !profile_dir.exists() {
            return Err(anyhow!("profile '{}' does not exist for {}", name, self));
        }

        std::fs::remove_dir_all(&profile_dir)?;
        Ok(())
    }

    pub fn order_file(&self) -> Result<PathBuf> {
        Ok(self.profiles_dir()?.join("_order"))
    }

    pub fn save_profile_order(&self, profiles: &[String]) -> Result<()> {
        let content = profiles.join("\n") + "\n";
        fs_util::atomic_write(&self.order_file()?, &content)
    }

    pub fn list_profiles(&self) -> Result<Vec<String>> {
        let profiles_dir = self.profiles_dir()?;
        if !profiles_dir.exists() {
            return Ok(vec![]);
        }
        let mut existing = HashSet::new();
        for entry in std::fs::read_dir(&profiles_dir)? {
            let entry = entry?;
            let Ok(name) = entry.file_name().into_string() else {
                continue;
            };
            if name == "_current" || name == "_order" {
                continue;
            }
            if entry.file_type()?.is_dir() {
                existing.insert(name);
            }
        }

        let order_content = std::fs::read_to_string(self.order_file()?).ok();
        Ok(merge_profiles_with_order(
            existing,
            order_content.as_deref(),
        ))
    }
}

fn merge_profiles_with_order(
    existing: HashSet<String>,
    order_content: Option<&str>,
) -> Vec<String> {
    let mut remaining = existing;
    let mut result = Vec::new();

    if let Some(content) = order_content {
        for line in content.lines() {
            let name = line.trim();
            if !name.is_empty() && remaining.remove(name) {
                result.push(name.to_string());
            }
        }
    }

    let mut rest: Vec<String> = remaining.into_iter().collect();
    rest.sort();
    result.extend(rest);

    result
}

impl fmt::Display for Tool {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Tool::Claude => write!(f, "Claude Code"),
            Tool::Codex => write!(f, "Codex CLI"),
        }
    }
}

impl std::str::FromStr for Tool {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        match s.to_lowercase().as_str() {
            "claude" => Ok(Tool::Claude),
            "codex" => Ok(Tool::Codex),
            _ => Err(anyhow!(
                "unknown tool: {} (expected 'claude' or 'codex')",
                s
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_dir_rejects_path_traversal() {
        assert!(Tool::Claude.profile_dir("../evil").is_err());
        assert!(Tool::Claude.profile_dir("foo/bar").is_err());
        assert!(Tool::Claude.profile_dir("foo\\bar").is_err());
        assert!(Tool::Claude.profile_dir("..").is_err());
        assert!(Tool::Claude.profile_dir(".").is_err());
        assert!(Tool::Claude.profile_dir("").is_err());
        assert!(Tool::Claude.profile_dir("foo\0bar").is_err());
        assert!(Tool::Claude.profile_dir(" leading").is_err());
        assert!(Tool::Claude.profile_dir("trailing ").is_err());
    }

    #[test]
    fn profile_dir_rejects_reserved_names() {
        assert!(Tool::Claude.profile_dir("_current").is_err());
        assert!(Tool::Codex.profile_dir("_current").is_err());
        assert!(Tool::Claude.profile_dir("_order").is_err());
        assert!(Tool::Codex.profile_dir("_order").is_err());
    }

    #[test]
    fn profile_dir_accepts_valid_names() {
        assert!(Tool::Claude.profile_dir("personal").is_ok());
        assert!(Tool::Claude.profile_dir("work-account").is_ok());
        assert!(Tool::Claude.profile_dir("test_123").is_ok());
    }

    #[test]
    fn merge_profiles_with_order_no_order_file() {
        let existing = HashSet::from(["c".to_string(), "a".to_string(), "b".to_string()]);
        let result = merge_profiles_with_order(existing, None);
        assert_eq!(result, vec!["a", "b", "c"]);
    }

    #[test]
    fn merge_profiles_with_order_respects_order() {
        let existing = HashSet::from(["a".to_string(), "b".to_string(), "c".to_string()]);
        let result = merge_profiles_with_order(existing, Some("c\nb\na\n"));
        assert_eq!(result, vec!["c", "b", "a"]);
    }

    #[test]
    fn merge_profiles_with_order_appends_new_profiles_alphabetically() {
        let existing = HashSet::from([
            "a".to_string(),
            "b".to_string(),
            "c".to_string(),
            "d".to_string(),
        ]);
        let result = merge_profiles_with_order(existing, Some("c\na\n"));
        assert_eq!(result, vec!["c", "a", "b", "d"]);
    }

    #[test]
    fn merge_profiles_with_order_ignores_deleted_profiles() {
        let existing = HashSet::from(["a".to_string(), "c".to_string()]);
        let result = merge_profiles_with_order(existing, Some("c\nb\na\n"));
        assert_eq!(result, vec!["c", "a"]);
    }

    #[test]
    fn merge_profiles_with_order_handles_empty_lines() {
        let existing = HashSet::from(["a".to_string(), "b".to_string()]);
        let result = merge_profiles_with_order(existing, Some("\n  \nb\n\na\n"));
        assert_eq!(result, vec!["b", "a"]);
    }
}
