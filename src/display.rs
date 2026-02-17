use chrono::{DateTime, Local, Utc};

const BAR_WIDTH: usize = 20;

pub enum DisplayMode {
    Used,
    Left,
}

pub fn render_bar(percent: f64) -> String {
    let filled = ((percent / 100.0) * BAR_WIDTH as f64).round() as usize;
    let filled = filled.min(BAR_WIDTH);
    let empty = BAR_WIDTH - filled;
    "\u{2588}".repeat(filled) + &"\u{2591}".repeat(empty)
}

pub fn format_usage_line(
    label: &str,
    percent: f64,
    resets_at: Option<DateTime<Utc>>,
    mode: &DisplayMode,
) -> String {
    let (display_percent, mode_label) = match mode {
        DisplayMode::Used => (percent, "used"),
        DisplayMode::Left => (100.0 - percent, "left"),
    };
    let reset_label = match resets_at {
        Some(reset_at) => format!("resets at {}", format_reset_time(reset_at)),
        None => "session not started".to_string(),
    };
    format!(
        "{}  {}  {:>5.1}% {}  {}",
        label,
        render_bar(display_percent),
        display_percent,
        mode_label,
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
