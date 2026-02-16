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
    Login,
}
