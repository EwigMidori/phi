//! phi-code CLI — TUI skeleton (enter / draw / quit / restore).

use std::io;

use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::widgets::Paragraph;

fn main() -> io::Result<()> {
    let mut terminal = ratatui::init();
    let mut result = Ok(());

    loop {
        if let Err(err) = terminal.draw(|frame| {
            let title = format!(
                "{} {}  (q quit)",
                phi_code_core::CORE_NAME,
                phi_code_core::version()
            );
            frame.render_widget(Paragraph::new(title).centered(), frame.area());
        }) {
            result = Err(err);
            break;
        }

        match event::read() {
            Ok(Event::Key(key))
                if key.kind == KeyEventKind::Press && key.code == KeyCode::Char('q') =>
            {
                break;
            }
            Ok(_) => {}
            Err(err) => {
                result = Err(err);
                break;
            }
        }
    }

    ratatui::restore();
    result
}
