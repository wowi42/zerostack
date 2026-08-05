use std::io::Write;

use crossterm::ExecutableCommand;
use crossterm::cursor::MoveTo;
use crossterm::event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::terminal::{self, Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen};

pub struct TerminalGuard {
    alternate_screen: bool,
    mouse_capture: bool,
}

impl TerminalGuard {
    pub fn new(alternate_screen: bool) -> std::io::Result<Self> {
        let mut stdout = std::io::stdout();
        if alternate_screen {
            stdout.execute(EnterAlternateScreen)?;
            stdout.execute(Clear(ClearType::All))?;
        } else {
            // Main-screen mode: paint the TUI frame directly on the terminal's
            // normal scrollback buffer. Clear the visible screen first so any
            // previous shell output is not mixed with the frame.
            stdout.execute(Clear(ClearType::All))?;
            stdout.execute(MoveTo(0, 0))?;
        }
        // When not using the alternate screen we want native terminal mouse
        // selection/scrollback, so leave mouse capture disabled.
        let mouse_capture = alternate_screen;
        if mouse_capture {
            stdout.execute(EnableMouseCapture)?;
        }
        stdout.execute(EnableBracketedPaste)?;
        let _ = stdout.execute(PushKeyboardEnhancementFlags(
            KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES,
        ));
        terminal::enable_raw_mode()?;
        Ok(TerminalGuard {
            alternate_screen,
            mouse_capture,
        })
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = terminal::disable_raw_mode();
        let mut stdout = std::io::stdout();
        let _ = stdout.execute(PopKeyboardEnhancementFlags);
        let _ = stdout.execute(DisableBracketedPaste);
        if self.mouse_capture {
            let _ = stdout.execute(DisableMouseCapture);
        }
        if self.alternate_screen {
            let _ = stdout.execute(LeaveAlternateScreen);
        } else {
            // Back in the main buffer: leave the rendered content in scrollback.
            // Print a newline so the shell prompt appears below the bottom region.
            let _ = writeln!(stdout);
        }
        let _ = stdout.flush();
    }
}
