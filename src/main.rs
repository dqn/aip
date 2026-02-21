mod claude;
mod cli;
mod codex;
mod display;
mod tool;

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use anyhow::Result;
use chrono::Local;
use clap::Parser;
use console::{Key, Term};
use dialoguer::{Input, Select};

use cli::{Cli, Command};
use codex::usage::RateLimits;
use display::{DisplayMode, format_usage_line};
use tool::Tool;

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse_from(cli::normalize_short_flags(std::env::args_os()));

    match cli.command {
        None => cmd_dashboard().await?,
        Some(Command::Save { tool, profile }) => cmd_save(tool, profile)?,
    }

    Ok(())
}

const USAGE_REFRESH_INTERVAL: Duration = Duration::from_secs(5);

#[derive(Clone, Debug, PartialEq)]
struct ProfileUsageCache {
    lines: Vec<String>,
    plan_type: Option<String>,
}

type UsageCache = HashMap<String, ProfileUsageCache>;

enum DashboardMode {
    Normal,
    DeleteConfirm(usize),
}

enum DashboardAction {
    None,
    Render,
    Reload,
    Quit,
}

struct ScreenGuard<'a>(&'a Term);

impl Drop for ScreenGuard<'_> {
    fn drop(&mut self) {
        let _ = self.0.show_cursor();
        let _ = self.0.write_str("\x1b[?1049l");
    }
}

// The blocking thread will keep waiting on `read_key()` after the receiver is dropped,
// only exiting once the next keypress unblocks it. This is a known limitation of
// blocking terminal reads without timeout support in the `console` crate.
fn spawn_key_reader() -> tokio::sync::mpsc::UnboundedReceiver<std::io::Result<Key>> {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    tokio::task::spawn_blocking(move || {
        let term = Term::stderr();
        loop {
            let key = term.read_key();
            if tx.send(key).is_err() {
                break;
            }
        }
    });
    rx
}

fn capitalize_first(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        None => String::new(),
        Some(c) => c.to_uppercase().to_string() + chars.as_str(),
    }
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

// --- Usage fetching ---

async fn prefetch_claude_usage() -> UsageCache {
    let results = claude::usage::fetch_all_profiles_usage().await;
    results
        .into_iter()
        .map(|(profile, result)| {
            let entry = match result {
                Ok((usage, info)) => ProfileUsageCache {
                    lines: vec![
                        format_usage_line(
                            "5-hour",
                            usage.five_hour.utilization,
                            usage.five_hour.resets_at,
                            &DisplayMode::Used,
                        ),
                        format_usage_line(
                            "Weekly",
                            usage.seven_day.utilization,
                            usage.seven_day.resets_at,
                            &DisplayMode::Used,
                        ),
                    ],
                    plan_type: info.plan_type,
                },
                Err(e) => ProfileUsageCache {
                    lines: vec![format!("Error: {}", e)],
                    plan_type: None,
                },
            };
            (profile, entry)
        })
        .collect()
}

fn codex_usage_lines(result: Result<Option<RateLimits>>) -> Vec<String> {
    match result {
        Ok(Some(limits)) => {
            let mut lines = Vec::new();
            if let Some(primary) = &limits.primary {
                lines.push(format_usage_line(
                    "5-hour",
                    primary.used_percent,
                    primary.resets_at_utc(),
                    &DisplayMode::Left,
                ));
            }
            if let Some(secondary) = &limits.secondary {
                lines.push(format_usage_line(
                    "Weekly",
                    secondary.used_percent,
                    secondary.resets_at_utc(),
                    &DisplayMode::Left,
                ));
            }
            if lines.is_empty() {
                vec!["No usage data available".to_string()]
            } else {
                lines
            }
        }
        Ok(None) => vec!["No usage data available".to_string()],
        Err(e) => vec![format!("Error: {}", e)],
    }
}

async fn prefetch_codex_usage(profiles: &[String]) -> UsageCache {
    let current = Tool::Codex.current_profile().ok().flatten();

    let mut handles = Vec::new();
    for p in profiles {
        let p = p.clone();
        let is_current = current.as_deref() == Some(p.as_str());
        handles.push(tokio::spawn(async move {
            let result = if is_current {
                codex::usage::fetch_usage().await
            } else {
                async {
                    let dir = Tool::Codex.profile_dir(&p)?;
                    let auth_path = dir.join("auth.json");
                    codex::usage::fetch_usage_from_auth(&auth_path).await
                }
                .await
            };
            (
                p,
                ProfileUsageCache {
                    lines: codex_usage_lines(result),
                    plan_type: None,
                },
            )
        }));
    }

    let mut results = HashMap::new();
    for handle in handles {
        if let Ok((p, entry)) = handle.await {
            results.insert(p, entry);
        }
    }
    results
}

// --- Dashboard ---

fn load_tool_profiles() -> Vec<(Tool, Vec<String>, Option<String>)> {
    Tool::ALL
        .iter()
        .map(|&t| {
            let profiles = t.list_profiles().unwrap_or_default();
            let current = t.current_profile().ok().flatten();
            (t, profiles, current)
        })
        .collect()
}

fn get_codex_profiles(tool_profiles: &[(Tool, Vec<String>, Option<String>)]) -> Vec<String> {
    tool_profiles
        .iter()
        .find(|(t, _, _)| *t == Tool::Codex)
        .map(|(_, p, _)| p.clone())
        .unwrap_or_default()
}

fn build_selectable_items(
    tool_profiles: &[(Tool, Vec<String>, Option<String>)],
) -> Vec<(Tool, String)> {
    tool_profiles
        .iter()
        .flat_map(|(tool, profiles, _)| profiles.iter().map(move |p| (*tool, p.clone())))
        .collect()
}

fn is_current_profile(
    tool_profiles: &[(Tool, Vec<String>, Option<String>)],
    tool: Tool,
    profile: &str,
) -> bool {
    tool_profiles
        .iter()
        .find(|(t, _, _)| *t == tool)
        .and_then(|(_, _, current)| current.as_deref())
        == Some(profile)
}

fn build_dashboard_lines(
    tool_profiles: &[(Tool, Vec<String>, Option<String>)],
    usage_caches: &HashMap<Tool, UsageCache>,
    pending_tools: &HashSet<Tool>,
    selectable_items: &[(Tool, String)],
    selected: usize,
    mode: &DashboardMode,
) -> Vec<String> {
    let mut lines = Vec::new();

    let header = if pending_tools.is_empty() {
        let timestamp = Local::now().format("%H:%M:%S");
        format!("aip - Usage Monitor (5s refresh)  Updated: {}", timestamp)
    } else {
        "aip - Usage Monitor (5s refresh)  Refreshing...".to_string()
    };
    lines.push(header);
    lines.push(String::new());

    let mut item_idx = 0;

    for (tool, profiles, current) in tool_profiles {
        lines.push(tool.to_string());

        if profiles.is_empty() {
            lines.push("  (no profiles)".to_string());
        } else {
            let cache = usage_caches.get(tool);
            for profile in profiles {
                let is_selected = item_idx < selectable_items.len() && item_idx == selected;
                let cursor = if is_selected { ">" } else { " " };
                let marker = if current.as_deref() == Some(profile.as_str()) {
                    " \x1b[32m✓\x1b[0m"
                } else {
                    ""
                };
                let plan_suffix = cache
                    .and_then(|c| c.get(profile))
                    .and_then(|entry| entry.plan_type.as_deref())
                    .map(|pt| format!(" ({})", capitalize_first(pt)))
                    .unwrap_or_default();
                let line = format!("{} {}{}{}", cursor, profile, marker, plan_suffix);
                if is_selected {
                    lines.push(format!("\x1b[1;36m{}\x1b[0m", line));
                } else {
                    lines.push(line);
                }

                if let Some(entry) = cache.and_then(|c| c.get(profile)) {
                    for line in &entry.lines {
                        lines.push(format!("    {}", line));
                    }
                } else if pending_tools.contains(tool) {
                    lines.push("    (loading...)".to_string());
                } else {
                    lines.push("    (no data)".to_string());
                }

                item_idx += 1;
            }
        }

        lines.push(String::new());
    }

    match mode {
        DashboardMode::Normal => {
            lines.push(
                "[↑↓] Navigate  [Enter/Space] Switch  [BS/Del] Delete  [ESC/q] Quit".to_string(),
            );
        }
        DashboardMode::DeleteConfirm(idx) => {
            if let Some((tool, profile)) = selectable_items.get(*idx) {
                lines.push(format!("Delete '{}' for {}? [y/n]", profile, tool));
            }
        }
    }

    lines
}

struct DashboardView<'a> {
    tool_profiles: &'a [(Tool, Vec<String>, Option<String>)],
    usage_caches: &'a HashMap<Tool, UsageCache>,
    pending_tools: &'a HashSet<Tool>,
    selectable_items: &'a [(Tool, String)],
    selected: usize,
    mode: &'a DashboardMode,
}

impl<'a> DashboardView<'a> {
    fn new(
        tool_profiles: &'a [(Tool, Vec<String>, Option<String>)],
        usage_caches: &'a HashMap<Tool, UsageCache>,
        pending_tools: &'a HashSet<Tool>,
        selectable_items: &'a [(Tool, String)],
        selected: usize,
        mode: &'a DashboardMode,
    ) -> Self {
        Self {
            tool_profiles,
            usage_caches,
            pending_tools,
            selectable_items,
            selected,
            mode,
        }
    }
}

fn render_dashboard(term: &Term, view: &DashboardView) -> Result<()> {
    term.write_str("\x1b[H")?;

    let lines = build_dashboard_lines(
        view.tool_profiles,
        view.usage_caches,
        view.pending_tools,
        view.selectable_items,
        view.selected,
        view.mode,
    );
    for line in &lines {
        term.write_str(line)?;
        term.write_str("\x1b[K\n")?;
    }
    term.write_str("\x1b[J")?;

    Ok(())
}

fn handle_dashboard_key(
    key: Key,
    selected: &mut usize,
    mode: &mut DashboardMode,
    selectable_items: &[(Tool, String)],
    tool_profiles: &[(Tool, Vec<String>, Option<String>)],
) -> DashboardAction {
    if selectable_items.is_empty() {
        return match key {
            Key::Escape | Key::Char('q') => DashboardAction::Quit,
            _ => DashboardAction::None,
        };
    }

    match mode {
        DashboardMode::Normal => match key {
            Key::ArrowUp => {
                *selected = selected.saturating_sub(1);
                DashboardAction::Render
            }
            Key::ArrowDown => {
                if *selected < selectable_items.len() - 1 {
                    *selected += 1;
                }
                DashboardAction::Render
            }
            Key::Enter | Key::Char(' ') => {
                let (tool, profile) = &selectable_items[*selected];
                if is_current_profile(tool_profiles, *tool, profile) {
                    return DashboardAction::None;
                }
                if cmd_switch(*tool, profile).is_ok() {
                    DashboardAction::Reload
                } else {
                    DashboardAction::None
                }
            }
            Key::Backspace | Key::Del => {
                let (tool, profile) = &selectable_items[*selected];
                if is_current_profile(tool_profiles, *tool, profile) {
                    return DashboardAction::None;
                }
                *mode = DashboardMode::DeleteConfirm(*selected);
                DashboardAction::Render
            }
            Key::Escape | Key::Char('q') => DashboardAction::Quit,
            _ => DashboardAction::None,
        },
        DashboardMode::DeleteConfirm(idx) => {
            let idx = *idx;
            match key {
                Key::Char('y') => {
                    let (tool, profile) = &selectable_items[idx];
                    let result = match tool {
                        Tool::Claude => claude::profile::delete(profile),
                        Tool::Codex => codex::profile::delete(profile),
                    };
                    *mode = DashboardMode::Normal;
                    if result.is_ok() {
                        DashboardAction::Reload
                    } else {
                        DashboardAction::Render
                    }
                }
                Key::Char('n') | Key::Escape => {
                    *mode = DashboardMode::Normal;
                    DashboardAction::Render
                }
                _ => DashboardAction::None,
            }
        }
    }
}

async fn cmd_dashboard() -> Result<()> {
    let term = Term::stderr();
    term.write_str("\x1b[?1049h")?;
    let _guard = ScreenGuard(&term);
    term.hide_cursor()?;

    let mut usage_caches: HashMap<Tool, UsageCache> = HashMap::new();
    let mut key_rx = spawn_key_reader();
    let mut selected: usize = 0;
    let mut mode = DashboardMode::Normal;

    loop {
        let tool_profiles = load_tool_profiles();
        let codex_profiles = get_codex_profiles(&tool_profiles);
        let selectable_items = build_selectable_items(&tool_profiles);

        if !selectable_items.is_empty() {
            selected = selected.min(selectable_items.len() - 1);
        } else {
            selected = 0;
        }

        let claude_future = prefetch_claude_usage();
        let codex_future = prefetch_codex_usage(&codex_profiles);
        tokio::pin!(claude_future);
        tokio::pin!(codex_future);

        let mut pending_tools: HashSet<Tool> = HashSet::from([Tool::Claude, Tool::Codex]);
        let mut claude_done = false;
        let mut codex_done = false;

        render_dashboard(
            &term,
            &DashboardView::new(
                &tool_profiles,
                &usage_caches,
                &pending_tools,
                &selectable_items,
                selected,
                &mode,
            ),
        )?;

        let mut needs_reload = false;

        // Phase 1: Wait for fetches + handle keys
        while !(claude_done && codex_done) {
            tokio::select! {
                cache = &mut claude_future, if !claude_done => {
                    usage_caches.insert(Tool::Claude, cache);
                    pending_tools.remove(&Tool::Claude);
                    claude_done = true;
                    render_dashboard(
                        &term,
                        &DashboardView::new(&tool_profiles, &usage_caches, &pending_tools, &selectable_items, selected, &mode),
                    )?;
                }
                cache = &mut codex_future, if !codex_done => {
                    usage_caches.insert(Tool::Codex, cache);
                    pending_tools.remove(&Tool::Codex);
                    codex_done = true;
                    render_dashboard(
                        &term,
                        &DashboardView::new(&tool_profiles, &usage_caches, &pending_tools, &selectable_items, selected, &mode),
                    )?;
                }
                Some(key_result) = key_rx.recv() => {
                    let key = match key_result {
                        Ok(k) => k,
                        Err(_) => continue,
                    };
                    match handle_dashboard_key(
                        key,
                        &mut selected,
                        &mut mode,
                        &selectable_items,
                        &tool_profiles,
                    ) {
                        DashboardAction::Quit => {
                            return Ok(());
                        }
                        DashboardAction::Reload => {
                            needs_reload = true;
                            break;
                        }
                        DashboardAction::Render => {
                            render_dashboard(
                                &term,
                                &DashboardView::new(&tool_profiles, &usage_caches, &pending_tools, &selectable_items, selected, &mode),
                            )?;
                        }
                        DashboardAction::None => {}
                    }
                }
            }
        }

        if needs_reload {
            continue;
        }

        // Phase 2: All data fetched — render final "Updated" state
        render_dashboard(
            &term,
            &DashboardView::new(
                &tool_profiles,
                &usage_caches,
                &pending_tools,
                &selectable_items,
                selected,
                &mode,
            ),
        )?;

        // Phase 3: Wait for refresh interval or user interaction
        loop {
            tokio::select! {
                Some(key_result) = key_rx.recv() => {
                    let key = match key_result {
                        Ok(k) => k,
                        Err(_) => continue,
                    };
                    match handle_dashboard_key(
                        key,
                        &mut selected,
                        &mut mode,
                        &selectable_items,
                        &tool_profiles,
                    ) {
                        DashboardAction::Quit => {
                            return Ok(());
                        }
                        DashboardAction::Reload => {
                            needs_reload = true;
                            break;
                        }
                        DashboardAction::Render => {
                            render_dashboard(
                                &term,
                                &DashboardView::new(&tool_profiles, &usage_caches, &pending_tools, &selectable_items, selected, &mode),
                            )?;
                        }
                        DashboardAction::None => {}
                    }
                }
                _ = tokio::time::sleep(USAGE_REFRESH_INTERVAL) => {
                    break;
                }
            }
        }

        if needs_reload {
            continue;
        }
    }
}

// --- CLI subcommands ---

fn cmd_switch(tool: Tool, profile: &str) -> Result<()> {
    match tool {
        Tool::Claude => claude::profile::switch(profile)?,
        Tool::Codex => codex::profile::switch(profile)?,
    }
    Ok(())
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

    match tool {
        Tool::Claude => claude::profile::save(&name)?,
        Tool::Codex => codex::profile::save(&name)?,
    }

    println!("Saved profile '{}' for {}", name, tool);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_entry(lines: Vec<String>, plan_type: Option<&str>) -> ProfileUsageCache {
        ProfileUsageCache {
            lines,
            plan_type: plan_type.map(String::from),
        }
    }

    fn sample_tool_profiles() -> Vec<(Tool, Vec<String>, Option<String>)> {
        vec![
            (
                Tool::Claude,
                vec!["personal".to_string(), "work".to_string()],
                Some("personal".to_string()),
            ),
            (
                Tool::Codex,
                vec!["dev".to_string()],
                Some("dev".to_string()),
            ),
        ]
    }

    #[test]
    fn build_selectable_items_creates_flat_list() {
        let tool_profiles = sample_tool_profiles();
        let items = build_selectable_items(&tool_profiles);

        assert_eq!(items.len(), 3);
        assert_eq!(items[0], (Tool::Claude, "personal".to_string()));
        assert_eq!(items[1], (Tool::Claude, "work".to_string()));
        assert_eq!(items[2], (Tool::Codex, "dev".to_string()));
    }

    #[test]
    fn build_selectable_items_empty_when_no_profiles() {
        let tool_profiles = vec![(Tool::Claude, vec![], None), (Tool::Codex, vec![], None)];
        let items = build_selectable_items(&tool_profiles);

        assert!(items.is_empty());
    }

    #[test]
    fn is_current_profile_returns_true_for_current() {
        let tool_profiles = sample_tool_profiles();

        assert!(is_current_profile(&tool_profiles, Tool::Claude, "personal"));
        assert!(is_current_profile(&tool_profiles, Tool::Codex, "dev"));
    }

    #[test]
    fn is_current_profile_returns_false_for_non_current() {
        let tool_profiles = sample_tool_profiles();

        assert!(!is_current_profile(&tool_profiles, Tool::Claude, "work"));
    }

    #[test]
    fn build_dashboard_lines_shows_cursor_on_selected_profile() {
        let tool_profiles = vec![(
            Tool::Claude,
            vec!["personal".to_string(), "work".to_string()],
            Some("personal".to_string()),
        )];
        let selectable_items = build_selectable_items(&tool_profiles);

        let lines = build_dashboard_lines(
            &tool_profiles,
            &HashMap::new(),
            &HashSet::new(),
            &selectable_items,
            1,
            &DashboardMode::Normal,
        );

        assert!(lines.iter().any(|l| l.starts_with("  personal")));
        assert!(lines.iter().any(|l| l.contains("> work")));
    }

    #[test]
    fn build_dashboard_lines_shows_no_profiles_when_empty() {
        let tool_profiles = vec![(Tool::Claude, vec![], None), (Tool::Codex, vec![], None)];
        let selectable_items = build_selectable_items(&tool_profiles);

        let lines = build_dashboard_lines(
            &tool_profiles,
            &HashMap::new(),
            &HashSet::new(),
            &selectable_items,
            0,
            &DashboardMode::Normal,
        );

        let no_profiles_count = lines.iter().filter(|l| l.contains("(no profiles)")).count();
        assert_eq!(no_profiles_count, 2);
    }

    #[test]
    fn build_dashboard_lines_marks_current_profile_with_asterisk() {
        let tool_profiles = vec![(
            Tool::Claude,
            vec!["personal".to_string(), "work".to_string()],
            Some("personal".to_string()),
        )];
        let selectable_items = build_selectable_items(&tool_profiles);

        let lines = build_dashboard_lines(
            &tool_profiles,
            &HashMap::new(),
            &HashSet::new(),
            &selectable_items,
            0,
            &DashboardMode::Normal,
        );

        assert!(
            lines
                .iter()
                .any(|l| l.contains("personal \x1b[32m✓\x1b[0m"))
        );
        assert!(!lines.iter().any(|l| l.contains("work \x1b[32m✓\x1b[0m")));
    }

    #[test]
    fn build_dashboard_lines_shows_no_data_when_usage_missing() {
        let tool_profiles = vec![(
            Tool::Claude,
            vec!["personal".to_string()],
            Some("personal".to_string()),
        )];
        let selectable_items = build_selectable_items(&tool_profiles);

        let lines = build_dashboard_lines(
            &tool_profiles,
            &HashMap::new(),
            &HashSet::new(),
            &selectable_items,
            0,
            &DashboardMode::Normal,
        );

        assert!(lines.iter().any(|l| l.contains("(no data)")));
    }

    #[test]
    fn build_dashboard_lines_displays_usage_data() {
        let tool_profiles = vec![(
            Tool::Claude,
            vec!["personal".to_string()],
            Some("personal".to_string()),
        )];
        let mut claude_cache: UsageCache = HashMap::new();
        claude_cache.insert(
            "personal".to_string(),
            make_entry(vec!["5-hour  60.0% used".to_string()], None),
        );
        let mut usage_caches = HashMap::new();
        usage_caches.insert(Tool::Claude, claude_cache);
        let selectable_items = build_selectable_items(&tool_profiles);

        let lines = build_dashboard_lines(
            &tool_profiles,
            &usage_caches,
            &HashSet::new(),
            &selectable_items,
            0,
            &DashboardMode::Normal,
        );

        assert!(lines.iter().any(|l| l.contains("60.0% used")));
        assert!(!lines.iter().any(|l| l.contains("(no data)")));
    }

    #[test]
    fn build_dashboard_lines_shows_loading_when_tool_pending() {
        let tool_profiles = vec![(
            Tool::Claude,
            vec!["personal".to_string()],
            Some("personal".to_string()),
        )];
        let pending_tools = HashSet::from([Tool::Claude]);
        let selectable_items = build_selectable_items(&tool_profiles);

        let lines = build_dashboard_lines(
            &tool_profiles,
            &HashMap::new(),
            &pending_tools,
            &selectable_items,
            0,
            &DashboardMode::Normal,
        );

        assert!(lines.iter().any(|l| l.contains("(loading...)")));
        assert!(!lines.iter().any(|l| l.contains("(no data)")));
    }

    #[test]
    fn build_dashboard_lines_header_shows_refreshing_when_pending() {
        let tool_profiles = vec![(Tool::Claude, vec![], None)];
        let pending_tools = HashSet::from([Tool::Claude]);
        let selectable_items = build_selectable_items(&tool_profiles);

        let lines = build_dashboard_lines(
            &tool_profiles,
            &HashMap::new(),
            &pending_tools,
            &selectable_items,
            0,
            &DashboardMode::Normal,
        );

        assert!(lines[0].contains("Refreshing..."));
        assert!(!lines[0].contains("Updated:"));
    }

    #[test]
    fn build_dashboard_lines_header_shows_updated_when_not_pending() {
        let tool_profiles = vec![(Tool::Claude, vec![], None)];
        let selectable_items = build_selectable_items(&tool_profiles);

        let lines = build_dashboard_lines(
            &tool_profiles,
            &HashMap::new(),
            &HashSet::new(),
            &selectable_items,
            0,
            &DashboardMode::Normal,
        );

        assert!(lines[0].contains("Updated:"));
        assert!(!lines[0].contains("Refreshing..."));
    }

    #[test]
    fn build_dashboard_lines_shows_plan_type() {
        let tool_profiles = vec![(
            Tool::Claude,
            vec!["personal".to_string()],
            Some("personal".to_string()),
        )];
        let mut claude_cache: UsageCache = HashMap::new();
        claude_cache.insert(
            "personal".to_string(),
            make_entry(vec!["5-hour  60.0% used".to_string()], Some("pro")),
        );
        let mut usage_caches = HashMap::new();
        usage_caches.insert(Tool::Claude, claude_cache);
        let selectable_items = build_selectable_items(&tool_profiles);

        let lines = build_dashboard_lines(
            &tool_profiles,
            &usage_caches,
            &HashSet::new(),
            &selectable_items,
            0,
            &DashboardMode::Normal,
        );

        assert!(
            lines
                .iter()
                .any(|l| l.contains("personal \x1b[32m✓\x1b[0m (Pro)"))
        );
    }

    #[test]
    fn build_dashboard_lines_hides_plan_type_when_none() {
        let tool_profiles = vec![(
            Tool::Claude,
            vec!["personal".to_string()],
            Some("personal".to_string()),
        )];
        let mut claude_cache: UsageCache = HashMap::new();
        claude_cache.insert(
            "personal".to_string(),
            make_entry(vec!["5-hour  60.0% used".to_string()], None),
        );
        let mut usage_caches = HashMap::new();
        usage_caches.insert(Tool::Claude, claude_cache);
        let selectable_items = build_selectable_items(&tool_profiles);

        let lines = build_dashboard_lines(
            &tool_profiles,
            &usage_caches,
            &HashSet::new(),
            &selectable_items,
            0,
            &DashboardMode::Normal,
        );

        let profile_line = lines
            .iter()
            .find(|l| l.contains("personal \x1b[32m✓\x1b[0m"))
            .unwrap();
        assert!(!profile_line.contains("("));
    }

    #[test]
    fn build_dashboard_lines_footer_shows_keybindings_in_normal_mode() {
        let tool_profiles = vec![(Tool::Claude, vec!["p".to_string()], Some("p".to_string()))];
        let selectable_items = build_selectable_items(&tool_profiles);

        let lines = build_dashboard_lines(
            &tool_profiles,
            &HashMap::new(),
            &HashSet::new(),
            &selectable_items,
            0,
            &DashboardMode::Normal,
        );

        let footer = lines.last().unwrap();
        assert!(footer.contains("Navigate"));
        assert!(footer.contains("Switch"));
        assert!(footer.contains("Delete"));
        assert!(footer.contains("Quit"));
    }

    #[test]
    fn build_dashboard_lines_footer_shows_confirm_in_delete_mode() {
        let tool_profiles = vec![(
            Tool::Claude,
            vec!["personal".to_string(), "work".to_string()],
            Some("personal".to_string()),
        )];
        let selectable_items = build_selectable_items(&tool_profiles);

        let lines = build_dashboard_lines(
            &tool_profiles,
            &HashMap::new(),
            &HashSet::new(),
            &selectable_items,
            1,
            &DashboardMode::DeleteConfirm(1),
        );

        let footer = lines.last().unwrap();
        assert!(footer.contains("Delete 'work' for Claude Code? [y/n]"));
    }

    #[test]
    fn capitalize_first_capitalizes_first_char() {
        assert_eq!(capitalize_first("pro"), "Pro");
        assert_eq!(capitalize_first(""), "");
        assert_eq!(capitalize_first("Pro"), "Pro");
        assert_eq!(capitalize_first("日本語"), "日本語");
    }

    #[test]
    fn handle_dashboard_key_navigates_with_arrow_keys() {
        let tool_profiles = sample_tool_profiles();
        let selectable_items = build_selectable_items(&tool_profiles);
        let mut selected = 0;
        let mut mode = DashboardMode::Normal;

        let action = handle_dashboard_key(
            Key::ArrowDown,
            &mut selected,
            &mut mode,
            &selectable_items,
            &tool_profiles,
        );
        assert!(matches!(action, DashboardAction::Render));
        assert_eq!(selected, 1);

        let action = handle_dashboard_key(
            Key::ArrowUp,
            &mut selected,
            &mut mode,
            &selectable_items,
            &tool_profiles,
        );
        assert!(matches!(action, DashboardAction::Render));
        assert_eq!(selected, 0);
    }

    #[test]
    fn handle_dashboard_key_does_not_navigate_past_bounds() {
        let tool_profiles = sample_tool_profiles();
        let selectable_items = build_selectable_items(&tool_profiles);
        let mut selected = 0;
        let mut mode = DashboardMode::Normal;

        handle_dashboard_key(
            Key::ArrowUp,
            &mut selected,
            &mut mode,
            &selectable_items,
            &tool_profiles,
        );
        assert_eq!(selected, 0);

        selected = selectable_items.len() - 1;
        handle_dashboard_key(
            Key::ArrowDown,
            &mut selected,
            &mut mode,
            &selectable_items,
            &tool_profiles,
        );
        assert_eq!(selected, selectable_items.len() - 1);
    }

    #[test]
    fn handle_dashboard_key_enter_on_current_profile_does_nothing() {
        let tool_profiles = sample_tool_profiles();
        let selectable_items = build_selectable_items(&tool_profiles);
        let mut selected = 0; // "personal" is current for Claude
        let mut mode = DashboardMode::Normal;

        let action = handle_dashboard_key(
            Key::Enter,
            &mut selected,
            &mut mode,
            &selectable_items,
            &tool_profiles,
        );
        assert!(matches!(action, DashboardAction::None));
    }

    #[test]
    fn handle_dashboard_key_backspace_on_current_profile_does_nothing() {
        let tool_profiles = sample_tool_profiles();
        let selectable_items = build_selectable_items(&tool_profiles);
        let mut selected = 0; // "personal" is current for Claude
        let mut mode = DashboardMode::Normal;

        let action = handle_dashboard_key(
            Key::Backspace,
            &mut selected,
            &mut mode,
            &selectable_items,
            &tool_profiles,
        );
        assert!(matches!(action, DashboardAction::None));
    }

    #[test]
    fn handle_dashboard_key_backspace_enters_delete_confirm() {
        let tool_profiles = sample_tool_profiles();
        let selectable_items = build_selectable_items(&tool_profiles);
        let mut selected = 1; // "work" is NOT current for Claude
        let mut mode = DashboardMode::Normal;

        let action = handle_dashboard_key(
            Key::Backspace,
            &mut selected,
            &mut mode,
            &selectable_items,
            &tool_profiles,
        );
        assert!(matches!(action, DashboardAction::Render));
        assert!(matches!(mode, DashboardMode::DeleteConfirm(1)));
    }

    #[test]
    fn handle_dashboard_key_escape_quits() {
        let tool_profiles = sample_tool_profiles();
        let selectable_items = build_selectable_items(&tool_profiles);
        let mut selected = 0;
        let mut mode = DashboardMode::Normal;

        let action = handle_dashboard_key(
            Key::Escape,
            &mut selected,
            &mut mode,
            &selectable_items,
            &tool_profiles,
        );
        assert!(matches!(action, DashboardAction::Quit));
    }

    #[test]
    fn handle_dashboard_key_q_quits() {
        let tool_profiles = sample_tool_profiles();
        let selectable_items = build_selectable_items(&tool_profiles);
        let mut selected = 0;
        let mut mode = DashboardMode::Normal;

        let action = handle_dashboard_key(
            Key::Char('q'),
            &mut selected,
            &mut mode,
            &selectable_items,
            &tool_profiles,
        );
        assert!(matches!(action, DashboardAction::Quit));
    }

    #[test]
    fn handle_dashboard_key_delete_confirm_n_cancels() {
        let tool_profiles = sample_tool_profiles();
        let selectable_items = build_selectable_items(&tool_profiles);
        let mut selected = 1;
        let mut mode = DashboardMode::DeleteConfirm(1);

        let action = handle_dashboard_key(
            Key::Char('n'),
            &mut selected,
            &mut mode,
            &selectable_items,
            &tool_profiles,
        );
        assert!(matches!(action, DashboardAction::Render));
        assert!(matches!(mode, DashboardMode::Normal));
    }

    #[test]
    fn handle_dashboard_key_delete_confirm_escape_cancels() {
        let tool_profiles = sample_tool_profiles();
        let selectable_items = build_selectable_items(&tool_profiles);
        let mut selected = 1;
        let mut mode = DashboardMode::DeleteConfirm(1);

        let action = handle_dashboard_key(
            Key::Escape,
            &mut selected,
            &mut mode,
            &selectable_items,
            &tool_profiles,
        );
        assert!(matches!(action, DashboardAction::Render));
        assert!(matches!(mode, DashboardMode::Normal));
    }

    #[test]
    fn handle_dashboard_key_quits_when_no_selectable_items() {
        let tool_profiles = vec![(Tool::Claude, vec![], None)];
        let selectable_items = build_selectable_items(&tool_profiles);
        let mut selected = 0;
        let mut mode = DashboardMode::Normal;

        let action = handle_dashboard_key(
            Key::Escape,
            &mut selected,
            &mut mode,
            &selectable_items,
            &tool_profiles,
        );
        assert!(matches!(action, DashboardAction::Quit));

        let action = handle_dashboard_key(
            Key::ArrowDown,
            &mut selected,
            &mut mode,
            &selectable_items,
            &tool_profiles,
        );
        assert!(matches!(action, DashboardAction::None));
    }
}
