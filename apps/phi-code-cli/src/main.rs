//! phi-code CLI — terminal host over [`AgentShell`].

mod prompt_pane;
mod scrollback_pane;
mod shell;
mod turn_driver;

use std::io::{self, stdout};
use std::time::Duration;

use crossterm::event::{
    self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
};
use crossterm::execute;
use shell::AgentShell;

fn main() -> io::Result<()> {
    run_terminal_host()
}

/// Own terminal lifecycle; all product behavior lives on [`AgentShell`].
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
