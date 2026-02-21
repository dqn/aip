mod claude;
mod cli;
mod codex;
mod dashboard;
mod display;
mod fs_util;
mod http;
mod tool;

use anyhow::Result;
use clap::Parser;
use dialoguer::{Confirm, Input, Select};

use cli::{Cli, Command};
use tool::Tool;

fn main() -> Result<()> {
    let rt = tokio::runtime::Runtime::new()?;
    let result = rt.block_on(async {
        let cli = Cli::parse_from(cli::normalize_short_flags(std::env::args_os()));

        match cli.command {
            None => dashboard::cmd_dashboard().await?,
            Some(Command::Save { tool, profile }) => cmd_save(tool, profile)?,
        }

        Ok(())
    });

    // Don't wait for the blocking key-reader thread to finish.
    rt.shutdown_background();

    result
}

fn select_tool() -> Result<Option<Tool>> {
    let items = ["Claude Code", "Codex CLI"];
    let selection = Select::new()
        .with_prompt("Select tool")
        .items(&items)
        .default(0)
        .interact_opt()?;

    Ok(selection.map(|i| Tool::ALL[i]))
}

fn cmd_save(tool_arg: Option<String>, profile_arg: Option<String>) -> Result<()> {
    let tool = match tool_arg {
        Some(t) => t.parse()?,
        None => {
            let Some(t) = select_tool()? else {
                return Ok(());
            };
            t
        }
    };

    let name = match profile_arg {
        Some(p) => p,
        None => Input::new().with_prompt("Profile name").interact_text()?,
    };

    if tool.profile_dir(&name)?.exists() {
        let confirmed = Confirm::new()
            .with_prompt(format!(
                "Profile '{}' already exists for {}. Overwrite?",
                name, tool
            ))
            .default(false)
            .interact()?;
        if !confirmed {
            return Ok(());
        }
    }

    match tool {
        Tool::Claude => claude::profile::save(&name)?,
        Tool::Codex => codex::profile::save(&name)?,
    }

    println!("Saved profile '{}' for {}", name, tool);
    Ok(())
}
