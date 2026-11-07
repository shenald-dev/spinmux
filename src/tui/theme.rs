use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::BorderType;

use crate::supervisor::ProcessStatus;

/// Terminal color rendering capability tier
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorCapability {
    /// 24-bit TrueColor RGB
    TrueColor,
    /// 8-bit ANSI 256 colors
    Ansi256,
    /// 4-bit standard ANSI 16 colors
    Ansi16,
}

impl Default for ColorCapability {
    fn default() -> Self {
        // Auto-detect based on COLORTERM environment variable
        if let Ok(val) = std::env::var("COLORTERM") {
            if val.eq_ignore_ascii_case("truecolor") || val.eq_ignore_ascii_case("24bit") {
                return ColorCapability::TrueColor;
            }
        }
        if let Ok(term) = std::env::var("TERM") {
            if term.contains("256color") {
                return ColorCapability::Ansi256;
            }
        }
        ColorCapability::TrueColor
    }
}

/// Resolves an RGB color to its closest ANSI-256 or 16-color representation based on capability
pub fn resolve_color(
    rgb: Color,
    fallback_256: u8,
    fallback_16: Color,
    cap: ColorCapability,
) -> Color {
    match cap {
        ColorCapability::TrueColor => rgb,
        ColorCapability::Ansi256 => Color::Indexed(fallback_256),
        ColorCapability::Ansi16 => fallback_16,
    }
}

/// Raw color palette definitions for spinmux Electric Cyan & Neon Violet theme
pub struct SpinmuxPalette;

impl SpinmuxPalette {
    // Primary Accents
    pub const ELECTRIC_CYAN: Color = Color::Rgb(0, 229, 255); // #00E5FF
    pub const NEON_VIOLET: Color = Color::Rgb(189, 0, 255); // #BD00FF
    pub const DEEP_VIOLET: Color = Color::Rgb(121, 40, 202); // #7928CA

    // Backgrounds & Surfaces
    pub const DARK_CHARCOAL: Color = Color::Rgb(18, 18, 20); // #121214
    pub const SURFACE_SLATE: Color = Color::Rgb(26, 26, 30); // #1A1A1E
    pub const SURFACE_HIGHLIGHT: Color = Color::Rgb(35, 35, 42); // #23232A

    // Status Indicators
    pub const STATUS_RUNNING: Color = Color::Rgb(0, 230, 118); // #00E676 Emerald Green
    pub const STATUS_STARTING: Color = Color::Rgb(255, 179, 0); // #FFB300 Amber
    pub const STATUS_STOPPED: Color = Color::Rgb(255, 23, 68); // #FF1744 Crimson
    pub const STATUS_SELECTED: Color = Color::Rgb(41, 121, 255); // #2979FF Electric Blue

    // Text & Neutrals
    pub const TEXT_PRIMARY: Color = Color::Rgb(255, 255, 255); // #FFFFFF
    pub const TEXT_MUTED: Color = Color::Rgb(160, 160, 170); // #A0A0AA
    pub const TEXT_DIM: Color = Color::Rgb(82, 82, 91); // #52525B
    pub const BORDER_NORMAL: Color = Color::Rgb(63, 63, 70); // #3F3F46
    pub const BORDER_FOCUSED: Color = Color::Rgb(0, 229, 255); // #00E5FF
    pub const ACCENT_YELLOW: Color = Color::Rgb(255, 215, 0); // #FFD700
}

/// Theme instance providing resolved styles and UI elements
#[derive(Debug, Clone, Copy, Default)]
pub struct Theme {
    pub capability: ColorCapability,
}

impl Theme {
    pub fn new(capability: ColorCapability) -> Self {
        Self { capability }
    }

    pub fn electric_cyan(&self) -> Color {
        resolve_color(
            SpinmuxPalette::ELECTRIC_CYAN,
            51,
            Color::Cyan,
            self.capability,
        )
    }

    pub fn neon_violet(&self) -> Color {
        resolve_color(
            SpinmuxPalette::NEON_VIOLET,
            129,
            Color::Magenta,
            self.capability,
        )
    }

    pub fn deep_violet(&self) -> Color {
        resolve_color(
            SpinmuxPalette::DEEP_VIOLET,
            135,
            Color::Magenta,
            self.capability,
        )
    }

    pub fn dark_charcoal(&self) -> Color {
        resolve_color(
            SpinmuxPalette::DARK_CHARCOAL,
            234,
            Color::Reset,
            self.capability,
        )
    }

    pub fn surface_slate(&self) -> Color {
        resolve_color(
            SpinmuxPalette::SURFACE_SLATE,
            235,
            Color::Black,
            self.capability,
        )
    }

    pub fn surface_highlight(&self) -> Color {
        resolve_color(
            SpinmuxPalette::SURFACE_HIGHLIGHT,
            236,
            Color::DarkGray,
            self.capability,
        )
    }

    pub fn status_running_color(&self) -> Color {
        resolve_color(
            SpinmuxPalette::STATUS_RUNNING,
            48,
            Color::Green,
            self.capability,
        )
    }

    pub fn status_starting_color(&self) -> Color {
        resolve_color(
            SpinmuxPalette::STATUS_STARTING,
            214,
            Color::Yellow,
            self.capability,
        )
    }

    pub fn status_stopped_color(&self) -> Color {
        resolve_color(
            SpinmuxPalette::STATUS_STOPPED,
            196,
            Color::Red,
            self.capability,
        )
    }

    pub fn status_selected_color(&self) -> Color {
        resolve_color(
            SpinmuxPalette::STATUS_SELECTED,
            33,
            Color::Blue,
            self.capability,
        )
    }

    pub fn text_primary(&self) -> Color {
        resolve_color(
            SpinmuxPalette::TEXT_PRIMARY,
            231,
            Color::White,
            self.capability,
        )
    }

    pub fn text_muted(&self) -> Color {
        resolve_color(
            SpinmuxPalette::TEXT_MUTED,
            247,
            Color::Gray,
            self.capability,
        )
    }

    pub fn text_dim(&self) -> Color {
        resolve_color(
            SpinmuxPalette::TEXT_DIM,
            240,
            Color::DarkGray,
            self.capability,
        )
    }

    pub fn border_normal(&self) -> Color {
        resolve_color(
            SpinmuxPalette::BORDER_NORMAL,
            238,
            Color::DarkGray,
            self.capability,
        )
    }

    pub fn border_focused(&self) -> Color {
        resolve_color(
            SpinmuxPalette::BORDER_FOCUSED,
            51,
            Color::Cyan,
            self.capability,
        )
    }

    pub fn border_type(&self) -> BorderType {
        BorderType::Rounded
    }

    pub fn status_symbol(&self, status: &ProcessStatus) -> &'static str {
        match status {
            ProcessStatus::Running(_) => "●",
            ProcessStatus::Starting => "◌",
            ProcessStatus::Stopped(_) => "✕",
            ProcessStatus::Crashing(_) => "◌",
            ProcessStatus::CrashLoopBackOff => "✕",
        }
    }

    pub fn status_color(&self, status: &ProcessStatus) -> Color {
        match status {
            ProcessStatus::Running(_) => self.status_running_color(),
            ProcessStatus::Starting | ProcessStatus::Crashing(_) => self.status_starting_color(),
            ProcessStatus::Stopped(_) | ProcessStatus::CrashLoopBackOff => {
                self.status_stopped_color()
            }
        }
    }

    pub fn status_style(&self, status: &ProcessStatus) -> Style {
        Style::default()
            .fg(self.status_color(status))
            .add_modifier(Modifier::BOLD)
    }

    pub fn header_logo_style(&self) -> Style {
        Style::default()
            .fg(self.neon_violet())
            .add_modifier(Modifier::BOLD)
    }

    pub fn header_proxy_style(&self) -> Style {
        Style::default()
            .fg(self.electric_cyan())
            .add_modifier(Modifier::BOLD)
    }

    pub fn block_style(&self, focused: bool) -> Style {
        if focused {
            Style::default().fg(self.border_focused())
        } else {
            Style::default().fg(self.border_normal())
        }
    }

    pub fn selected_row_style(&self) -> Style {
        Style::default()
            .bg(self.surface_highlight())
            .fg(self.electric_cyan())
            .add_modifier(Modifier::BOLD)
    }

    pub fn key_bracket_style(&self) -> Style {
        Style::default()
            .fg(self.neon_violet())
            .add_modifier(Modifier::BOLD)
    }

    pub fn key_char_style(&self) -> Style {
        Style::default()
            .fg(self.electric_cyan())
            .add_modifier(Modifier::BOLD)
    }

    pub fn key_desc_style(&self) -> Style {
        Style::default().fg(self.text_primary())
    }

    pub fn autoscroll_on_style(&self) -> Style {
        Style::default()
            .fg(self.status_running_color())
            .add_modifier(Modifier::BOLD)
    }

    pub fn autoscroll_paused_style(&self) -> Style {
        Style::default()
            .fg(Color::Rgb(18, 18, 20))
            .bg(self.status_starting_color())
            .add_modifier(Modifier::BOLD)
    }
}
