use std::fmt;
use std::path::PathBuf;

use anyhow::{Result, anyhow};

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
        Ok(Some(trimmed.to_string()))
    }

    pub fn profile_dir(&self, name: &str) -> Result<PathBuf> {
        if name.contains('/')
            || name.contains('\\')
            || name == ".."
            || name == "."
            || name.is_empty()
        {
            return Err(anyhow!("invalid profile name: '{}'", name));
        }
        Ok(self.profiles_dir()?.join(name))
    }

    pub fn list_profiles(&self) -> Result<Vec<String>> {
        let profiles_dir = self.profiles_dir()?;
        if !profiles_dir.exists() {
            return Ok(vec![]);
        }
        let mut profiles = Vec::new();
        for entry in std::fs::read_dir(&profiles_dir)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().to_string();
            if name == "_current" {
                continue;
            }
            if entry.file_type()?.is_dir() {
                profiles.push(name);
            }
        }
        profiles.sort();
        Ok(profiles)
    }
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
    }

    #[test]
    fn profile_dir_accepts_valid_names() {
        assert!(Tool::Claude.profile_dir("personal").is_ok());
        assert!(Tool::Claude.profile_dir("work-account").is_ok());
        assert!(Tool::Claude.profile_dir("test_123").is_ok());
    }
}
