use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(about = "AI Profile Manager - manage profiles for Claude Code and Codex CLI")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Subcommand)]
pub enum Command {
    /// Save current credentials as a new profile
    Save {
        /// Tool name (claude or codex)
        tool: Option<String>,
        /// Profile name
        profile: Option<String>,
    },
    /// Show usage for all tools
    Usage,
    /// List all profiles
    List,
    /// Show current profile for each tool
    Current,
    /// Delete a profile
    Delete {
        /// Tool name (claude or codex)
        tool: Option<String>,
        /// Profile name
        profile: Option<String>,
    },
    /// Switch profile (non-interactive)
    Switch {
        /// Tool name (claude or codex)
        tool: String,
        /// Profile name
        profile: String,
    },
    /// Log in and save credentials to a profile
    Login {
        /// Tool name (claude or codex)
        tool: Option<String>,
        /// Profile name
        profile: Option<String>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn login_command_accepts_tool_and_name_as_positional_args() {
        let parsed = Cli::try_parse_from(["aip", "login", "claude", "work"]);

        assert!(parsed.is_ok());
        assert!(matches!(
            parsed.unwrap().command,
            Some(Command::Login {
                tool: Some(tool),
                profile: Some(profile),
            }) if tool == "claude" && profile == "work"
        ));
    }

    #[test]
    fn login_command_accepts_only_tool_as_positional_arg() {
        let parsed = Cli::try_parse_from(["aip", "login", "codex"]);

        assert!(parsed.is_ok());
        assert!(matches!(
            parsed.unwrap().command,
            Some(Command::Login {
                tool: Some(tool),
                profile: None,
            }) if tool == "codex"
        ));
    }

    #[test]
    fn login_command_without_args_is_still_supported() {
        let parsed = Cli::try_parse_from(["aip", "login"]);

        assert!(parsed.is_ok());
        assert!(matches!(
            parsed.unwrap().command,
            Some(Command::Login {
                tool: None,
                profile: None,
            })
        ));
    }
}
