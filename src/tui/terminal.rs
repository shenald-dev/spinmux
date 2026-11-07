use std::io::{self, stdout, Stdout};
use std::sync::atomic::{AtomicBool, Ordering};

use crossterm::{
    cursor::{Hide, Show},
    event::{DisableMouseCapture, EnableMouseCapture},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, Terminal};

static TERMINAL_RESTORED: AtomicBool = AtomicBool::new(false);

/// RAII Terminal Guard ensuring clean terminal raw mode restoration on normal exit or panic
pub struct TerminalGuard {
    active: bool,
}

impl TerminalGuard {
    /// Initializes terminal raw mode, alternate screen buffer, and registers dual panic hook
    pub fn init() -> io::Result<(Terminal<CrosstermBackend<Stdout>>, Self)> {
        enable_raw_mode()?;
        let mut stdout = stdout();
        execute!(stdout, EnterAlternateScreen, EnableMouseCapture, Hide)?;
        let backend = CrosstermBackend::new(stdout);
        let terminal = Terminal::new(backend)?;

        TERMINAL_RESTORED.store(false, Ordering::SeqCst);

        // Register custom panic hook that restores raw mode before printing panic trace
        let default_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |panic_info| {
            let _ = Self::restore_terminal();
            default_hook(panic_info);
        }));

        Ok((terminal, TerminalGuard { active: true }))
    }

    /// Restores terminal from raw mode back to canonical mode, showing cursor and leaving alternate screen
    pub fn restore_terminal() -> io::Result<()> {
        if TERMINAL_RESTORED.swap(true, Ordering::SeqCst) {
            return Ok(());
        }

        let _ = disable_raw_mode();
        let mut stdout = stdout();
        let _ = execute!(stdout, LeaveAlternateScreen, DisableMouseCapture, Show);
        Ok(())
    }

    /// Disarms the guard if terminal was already manually restored
    pub fn deactivate(&mut self) {
        self.active = false;
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        if self.active {
            let _ = Self::restore_terminal();
            self.active = false;
        }
    }
}
