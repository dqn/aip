mod claude;
mod cli;
mod codex;
mod display;
mod tool;

use anyhow::Result;
use clap::Parser;
use dialoguer::{Confirm, Input, Select};

use cli::{Cli, Command};
use display::{format_reset_time, render_bar};
use tool::Tool;

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        None => cmd_interactive().await?,
        Some(Command::Save) => cmd_save()?,
        Some(Command::Usage) => cmd_usage().await?,
        Some(Command::List) => cmd_list()?,
        Some(Command::Current) => cmd_current()?,
        Some(Command::Delete) => cmd_delete()?,
        Some(Command::Switch { tool, profile }) => {
            let tool: Tool = tool.parse()?;
            cmd_switch(tool, &profile)?;
        }
    }

    Ok(())
}

fn select_tool() -> Result<Tool> {
    let items = ["Claude Code", "Codex CLI"];
    let selection = Select::new()
        .with_prompt("Select tool")
        .items(&items)
        .default(0)
        .interact()?;

    Ok(Tool::ALL[selection])
}

fn select_profile(tool: Tool) -> Result<String> {
    let profiles = tool.list_profiles()?;
    if profiles.is_empty() {
        anyhow::bail!("no profiles found for {}", tool);
    }

    let current = tool.current_profile()?;
    let display_items: Vec<String> = profiles
        .iter()
        .map(|p| {
            if current.as_deref() == Some(p.as_str()) {
                format!("{} (current)", p)
            } else {
                p.clone()
            }
        })
        .collect();

    let selection = Select::new()
        .with_prompt("Select profile")
        .items(&display_items)
        .default(0)
        .interact()?;

    Ok(profiles[selection].clone())
}

async fn cmd_interactive() -> Result<()> {
    let tool = select_tool()?;

    // Show current usage
    print_usage_for_tool(tool).await;

    // Select and switch profile
    let profile = select_profile(tool)?;

    if tool.current_profile()?.as_deref() == Some(profile.as_str()) {
        println!("Already on profile '{}'", profile);
        return Ok(());
    }

    cmd_switch(tool, &profile)?;
    println!("Switched {} to profile '{}'", tool, profile);

    Ok(())
}

fn cmd_save() -> Result<()> {
    let tool = select_tool()?;

    let name: String = Input::new().with_prompt("Profile name").interact_text()?;

    match tool {
        Tool::Claude => claude::profile::save(&name)?,
        Tool::Codex => codex::profile::save(&name)?,
    }

    println!("Saved profile '{}' for {}", name, tool);
    Ok(())
}

async fn cmd_usage() -> Result<()> {
    for tool in Tool::ALL {
        print_usage_for_tool(tool).await;
    }
    Ok(())
}

async fn print_usage_for_tool(tool: Tool) {
    let current = tool.current_profile().ok().flatten();
    let label = match &current {
        Some(name) => format!("{} [{}]", tool, name),
        None => format!("{}", tool),
    };

    match tool {
        Tool::Claude => print_claude_usage(&label).await,
        Tool::Codex => print_codex_usage(&label),
    }
}

async fn print_claude_usage(label: &str) {
    match claude::usage::fetch_usage().await {
        Ok((usage, info)) => {
            let suffix = match info.plan_type.as_deref() {
                Some(plan) => format!(" ({})", plan),
                None => String::new(),
            };
            println!("{}{}", label, suffix);
            println!(
                "  5-hour  {}  {:>5.1}%  resets at {}",
                render_bar(usage.five_hour.utilization),
                usage.five_hour.utilization,
                format_reset_time(usage.five_hour.resets_at),
            );
            println!(
                "  Weekly  {}  {:>5.1}%  resets at {}",
                render_bar(usage.seven_day.utilization),
                usage.seven_day.utilization,
                format_reset_time(usage.seven_day.resets_at),
            );
        }
        Err(e) => {
            println!("{}", label);
            println!("  Error: {}", e);
        }
    }
    println!();
}

fn print_codex_usage(label: &str) {
    match codex::usage::fetch_usage() {
        Ok(Some(limits)) => {
            println!("{}", label);
            if let Some(primary) = &limits.primary {
                println!(
                    "  5-hour  {}  {:>5.1}%  resets at {}",
                    render_bar(primary.used_percent),
                    primary.used_percent,
                    format_reset_time(primary.resets_at_utc()),
                );
            }
            if let Some(secondary) = &limits.secondary {
                println!(
                    "  Weekly  {}  {:>5.1}%  resets at {}",
                    render_bar(secondary.used_percent),
                    secondary.used_percent,
                    format_reset_time(secondary.resets_at_utc()),
                );
            }
        }
        Ok(None) => {
            println!("{}", label);
            println!("  No usage data available");
        }
        Err(e) => {
            println!("{}", label);
            println!("  Error: {}", e);
        }
    }
    println!();
}

fn cmd_list() -> Result<()> {
    for tool in Tool::ALL {
        let profiles = tool.list_profiles()?;
        let current = tool.current_profile()?;

        println!("{}:", tool);
        if profiles.is_empty() {
            println!("  (no profiles)");
        } else {
            for p in &profiles {
                let marker = if current.as_deref() == Some(p.as_str()) {
                    " *"
                } else {
                    ""
                };
                println!("  {}{}", p, marker);
            }
        }
        println!();
    }
    Ok(())
}

fn cmd_current() -> Result<()> {
    for tool in Tool::ALL {
        let current = tool.current_profile()?;
        match current {
            Some(name) => println!("{}: {}", tool, name),
            None => println!("{}: (none)", tool),
        }
    }
    Ok(())
}

fn cmd_switch(tool: Tool, profile: &str) -> Result<()> {
    match tool {
        Tool::Claude => claude::profile::switch(profile)?,
        Tool::Codex => codex::profile::switch(profile)?,
    }
    Ok(())
}

fn cmd_delete() -> Result<()> {
    let tool = select_tool()?;
    let profile = select_profile(tool)?;

    if !Confirm::new()
        .with_prompt(format!("Delete profile '{}' for {}?", profile, tool))
        .default(false)
        .interact()?
    {
        println!("Cancelled");
        return Ok(());
    }

    match tool {
        Tool::Claude => claude::profile::delete(&profile)?,
        Tool::Codex => codex::profile::delete(&profile)?,
    }

    println!("Deleted profile '{}' for {}", profile, tool);
    Ok(())
}
