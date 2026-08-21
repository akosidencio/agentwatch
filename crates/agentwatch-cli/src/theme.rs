//! One palette, two surfaces.
//!
//! The TUI and the plain listings are the same tool, so they name their colours
//! in the same place. Everything here is drawn from the 256-colour cube rather
//! than the named sixteen: the greys are the one part of that cube a terminal
//! theme does not remap, so chrome stays chrome instead of turning into
//! whatever the user's theme calls "black".
//!
//! Colour in the plain listings is a courtesy to a human reading a terminal,
//! never part of the output contract. It is off whenever stdout is not a
//! terminal, so `agentwatch sessions | grep` sees exactly the bytes it saw
//! before this module existed.

use std::io::IsTerminal as _;
use std::sync::OnceLock;

use ratatui::style::{Color, Modifier, Style};

/// Borders and rules. Present, but never competing with the data.
pub(crate) const FAINT: u8 = 238;
/// Column headers, units, and labels — the words around the numbers.
pub(crate) const MUTED: u8 = 245;
/// The one accent. Used for identity: session ids, projects, headings.
pub(crate) const ACCENT: u8 = 74;
/// A second hue, for categorical distinctions only — never for severity.
pub(crate) const VIOLET: u8 = 140;
/// A third hue, same rule.
pub(crate) const TEAL: u8 = 73;
/// Running, present, healthy.
pub(crate) const GOOD: u8 = 114;
/// Degraded, or true but not yet acted on.
pub(crate) const WARN: u8 = 179;
/// Sensitive access and anything the user is meant to look at twice.
pub(crate) const BAD: u8 = 167;

/// A ratatui style for one palette entry.
pub(crate) fn style(index: u8) -> Style {
    Style::default().fg(Color::Indexed(index))
}

/// A ratatui style for one palette entry, emphasised.
pub(crate) fn bold_style(index: u8) -> Style {
    style(index).add_modifier(Modifier::BOLD)
}

/// Whether the plain listings should emit escape sequences.
///
/// Resolved once. The order is the conventional one: an explicit `NO_COLOR`
/// beats everything, `CLICOLOR_FORCE` overrides the terminal check for people
/// piping into a pager that understands colour, and otherwise a pipe means no.
fn enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| {
        if std::env::var_os("NO_COLOR").is_some_and(|value| !value.is_empty()) {
            return false;
        }
        if std::env::var_os("CLICOLOR_FORCE").is_some_and(|value| !value.is_empty()) {
            return true;
        }
        std::io::stdout().is_terminal()
    })
}

/// Wraps text in one palette colour, or returns it untouched.
///
/// Takes the text already padded to its column width. Padding a string that
/// carries escape sequences counts those bytes as characters and pulls every
/// column after it out of line, so callers format first and colour second.
pub(crate) fn paint(text: &str, index: u8) -> String {
    paint_when(text, index, enabled())
}

/// Emphasises text without changing its colour.
pub(crate) fn bold(text: &str) -> String {
    bold_when(text, enabled())
}

/// [`paint`] with the terminal check supplied, so it can be tested either way.
fn paint_when(text: &str, index: u8, enabled: bool) -> String {
    if !enabled {
        return text.to_owned();
    }
    format!("\x1b[38;5;{index}m{text}\x1b[0m")
}

/// [`bold`] with the terminal check supplied, so it can be tested either way.
fn bold_when(text: &str, enabled: bool) -> String {
    if !enabled {
        return text.to_owned();
    }
    format!("\x1b[1m{text}\x1b[0m")
}

/// A horizontal rule, in the border colour.
pub(crate) fn rule(width: usize) -> String {
    paint(&"─".repeat(width), FAINT)
}

/// A proportion drawn as a bar, for share columns.
///
/// Eighth-blocks rather than a coarse `#` count: at four columns wide a whole
/// character is 25% of the scale, which is too much rounding to put next to a
/// percentage the reader can see is different.
pub(crate) fn bar(fraction: f64, width: usize) -> String {
    let clamped = fraction.clamp(0.0, 1.0);
    let eighths = (clamped * width as f64 * 8.0).round() as usize;
    let full = eighths / 8;
    let remainder = eighths % 8;

    let mut out = "█".repeat(full.min(width));
    if full < width && remainder > 0 {
        // Index is in 1..=7, so the partial-block characters, ▏ through ▉.
        out.push(['▏', '▎', '▍', '▌', '▋', '▊', '▉'][remainder - 1]);
    }
    let drawn = out.chars().count();
    for _ in drawn..width {
        out.push(' ');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bar_is_always_exactly_the_requested_width() {
        for percent in 0..=100 {
            let drawn = bar(f64::from(percent) / 100.0, 5);
            assert_eq!(drawn.chars().count(), 5, "{percent}% -> {drawn:?}");
        }
    }

    #[test]
    fn bar_clamps_rather_than_overflowing() {
        assert_eq!(bar(2.0, 4).chars().count(), 4);
        assert_eq!(bar(-1.0, 4), "    ");
        assert_eq!(bar(1.0, 4), "████");
    }

    #[test]
    fn painting_is_a_no_op_when_colour_is_off() {
        // The contract a pipe depends on: byte-identical to the unpainted text.
        assert_eq!(paint_when("running", GOOD, false), "running");
        assert_eq!(bold_when("total", false), "total");
    }

    #[test]
    fn painting_wraps_without_disturbing_the_text() {
        let painted = paint_when("running", GOOD, true);
        assert!(painted.contains("running"), "{painted:?}");
        assert!(painted.ends_with("\x1b[0m"), "{painted:?}");
        // Every listing test greps for substrings; a code spliced into the
        // middle of a word would pass here and break those.
        assert_eq!(painted.replace('\u{1b}', "|"), "|[38;5;114mrunning|[0m");
    }
}
