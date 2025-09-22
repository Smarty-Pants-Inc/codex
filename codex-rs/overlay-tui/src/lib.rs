//! TUI overlay scaffolding for Codex upstream fork.

use ratatui::style::{Color, Style};

pub fn branded_header_style() -> Style {
    Style::default().fg(Color::LightCyan)
}
