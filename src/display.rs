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
    let percent = percent.clamp(0.0, 100.0);
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
    use super::*;

    #[test]
    fn format_usage_line_handles_session_not_started() {
        let line = format_usage_line("5-hour", 0.0, None, &DisplayMode::Used);

        assert!(line.contains("session not started"));
    }

    #[test]
    fn render_bar_zero_percent() {
        let bar = render_bar(0.0, "\x1b[32m");
        assert!(!bar.contains('\u{2588}'));
        assert_eq!(bar.matches('\u{2591}').count(), BAR_WIDTH);
    }

    #[test]
    fn render_bar_full_percent() {
        let bar = render_bar(100.0, "\x1b[32m");
        assert_eq!(bar.matches('\u{2588}').count(), BAR_WIDTH);
        assert!(!bar.contains('\u{2591}'));
    }

    #[test]
    fn render_bar_clamps_negative() {
        let bar = render_bar(-10.0, "\x1b[32m");
        assert!(!bar.contains('\u{2588}'));
        assert_eq!(bar.matches('\u{2591}').count(), BAR_WIDTH);
    }

    #[test]
    fn render_bar_clamps_over_100() {
        let bar = render_bar(150.0, "\x1b[32m");
        assert_eq!(bar.matches('\u{2588}').count(), BAR_WIDTH);
        assert!(!bar.contains('\u{2591}'));
    }

    #[test]
    fn danger_color_green_for_low() {
        assert_eq!(danger_color(0.0), "\x1b[32m");
        assert_eq!(danger_color(50.0), "\x1b[32m");
    }

    #[test]
    fn danger_color_yellow_for_medium() {
        assert_eq!(danger_color(51.0), "\x1b[33m");
        assert_eq!(danger_color(80.0), "\x1b[33m");
    }

    #[test]
    fn danger_color_red_for_high() {
        assert_eq!(danger_color(81.0), "\x1b[31m");
        assert_eq!(danger_color(100.0), "\x1b[31m");
    }

    #[test]
    fn format_reset_time_different_day() {
        use chrono::TimeZone;
        let far_future = Utc.with_ymd_and_hms(2099, 12, 31, 12, 0, 0).unwrap();
        let result = format_reset_time(far_future);
        assert!(result.contains("Dec 31"));
    }
}
