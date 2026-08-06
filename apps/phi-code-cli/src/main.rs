//! phi-code CLI — terminal host.
//!
//! Layers: `phi-code-core` (application) · [`shell`] (UI chrome) · this file (I/O loop).
//! Process env (`PHI_*`) is loaded via [`process_env`]. Tokio hosts LLM stream tasks;
//! the TUI loop stays tick-driven.

mod process_env;
mod shell;

use std::io::{self, stdout};
use std::time::Duration;

use crossterm::event::{
    self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
};
use crossterm::execute;
use shell::AgentShell;

#[tokio::main]
async fn main() -> io::Result<()> {
    // Only the process cwd's `.env` — never walk parent monorepo dirs.
    let _ = dotenvy::dotenv();
    run_terminal_host()
}

/// Own terminal lifecycle; all product behavior lives on [`AgentShell`].
///
/// Must run inside a Tokio runtime so [`phi_code_core::SessionHost`] can
/// `tokio::spawn` the kernel SendQueue pump.
fn run_terminal_host() -> io::Result<()> {
    let mut terminal = ratatui::init();
    execute!(stdout(), EnableBracketedPaste, EnableMouseCapture)?;

    let mut shell = AgentShell::new();
    let mut result = Ok(());

    loop {
        shell.tick();

        if let Err(err) = terminal.draw(|frame| shell.draw(frame)) {
            result = Err(err);
            break;
        }

        if shell.should_quit() {
            break;
        }

        if !event::poll(Duration::from_millis(16))? {
            continue;
        }

        match event::read() {
            Ok(ev) => shell.handle(ev),
            Err(err) => {
                result = Err(err);
                break;
            }
        }

        if shell.should_quit() {
            break;
        }
    }

    let _ = execute!(stdout(), DisableMouseCapture, DisableBracketedPaste);
    ratatui::restore();
    result
}
