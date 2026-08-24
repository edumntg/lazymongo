//! Terminal lifecycle: raw mode, alternate screen, mouse capture, and a
//! panic hook that always restores the user's terminal (NFR-8).

use std::io::{self, Stdout};

use anyhow::Result;
use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::event::{
    DisableMouseCapture, EnableMouseCapture, KeyboardEnhancementFlags, PopKeyboardEnhancementFlags,
    PushKeyboardEnhancementFlags,
};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::Terminal;

pub type Term = Terminal<CrosstermBackend<Stdout>>;

pub fn init() -> Result<Term> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    push_key_enhancements();
    Ok(Terminal::new(CrosstermBackend::new(stdout))?)
}

/// Enable the kitty keyboard protocol where supported (Ghostty, Kitty,
/// WezTerm, foot, ...) so modifiers on Enter are reported — this is what
/// makes Shift+Enter distinguishable for "run" shortcuts. Terminals without
/// support just deliver a plain Enter.
fn push_key_enhancements() {
    // supports_keyboard_enhancement() probes the terminal and, as a side
    // effect, can leave raw mode disabled — which silently breaks all input
    // (the pty reverts to canonical line buffering). Re-assert raw mode
    // right after the probe.
    let supported = matches!(
        ratatui::crossterm::terminal::supports_keyboard_enhancement(),
        Ok(true)
    );
    let _ = enable_raw_mode();
    if supported {
        let _ = execute!(
            io::stdout(),
            PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
        );
    }
}

pub fn restore() {
    // Best-effort: never fail during shutdown/panic. Popping enhancement
    // flags on terminals that never pushed them is ignored per the spec.
    let _ = execute!(io::stdout(), PopKeyboardEnhancementFlags);
    let _ = disable_raw_mode();
    let _ = execute!(io::stdout(), LeaveAlternateScreen, DisableMouseCapture);
}

/// Re-enter the TUI after a suspend (e.g. returning from mongosh).
pub fn reenter(terminal: &mut Term) -> Result<()> {
    enable_raw_mode()?;
    execute!(io::stdout(), EnterAlternateScreen, EnableMouseCapture)?;
    push_key_enhancements();
    terminal.clear()?; // force a full redraw
    Ok(())
}

pub fn install_panic_hook() {
    let original = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore();
        original(info);
    }));
}
