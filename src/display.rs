use chrono::{DateTime, Local, Utc};

const BAR_WIDTH: usize = 20;
const RESET: &str = "\x1b[0m";
const CYAN: &str = "\x1b[36m";

fn danger_color(used_percent: f64) -> &'static str {
    if used_percent > 80.0 {
        "\x1b[31m" // red
    } else if used_percent > 50.0 {
        "\x1b[33m" // yellow
    } else {
        "\x1b[32m" // green
    }
}

pub enum DisplayMode {
    Used,
    Left,
}

pub fn render_bar(percent: f64, color: &str) -> String {
    let filled = ((percent / 100.0) * BAR_WIDTH as f64).round() as usize;
    let filled = filled.min(BAR_WIDTH);
    let empty = BAR_WIDTH - filled;
    format!(
        "{}{}{}{}",
        color,
        "\u{2588}".repeat(filled),
        RESET,
        "\u{2591}".repeat(empty),
    )
}

pub fn format_usage_line(
    label: &str,
    percent: f64,
    resets_at: Option<DateTime<Utc>>,
    mode: &DisplayMode,
) -> String {
    let color = danger_color(percent);
    let (display_percent, colored_mode_label) = match mode {
        DisplayMode::Used => (percent, format!("{color}used{RESET}")),
        DisplayMode::Left => (100.0 - percent, format!("{CYAN}left{RESET}")),
    };
    let reset_label = match resets_at {
        Some(reset_at) => format!("resets at {}", format_reset_time(reset_at)),
        None => "session not started".to_string(),
    };
    format!(
        "{}  {}  {:>5.1}% {}  {}",
        label,
        render_bar(display_percent, color),
        display_percent,
        colored_mode_label,
        reset_label,
    )
}

pub fn format_reset_time(reset_utc: DateTime<Utc>) -> String {
    let local: DateTime<Local> = reset_utc.into();
    let now = Local::now();

    if local.date_naive() == now.date_naive() {
        local.format("%H:%M").to_string()
    } else {
        local.format("%b %d %H:%M").to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::{DisplayMode, format_usage_line};

    #[test]
    fn format_usage_line_handles_session_not_started() {
        let line = format_usage_line("5-hour", 0.0, None, &DisplayMode::Used);

        assert!(line.contains("session not started"));
    }
}
