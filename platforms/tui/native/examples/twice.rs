//! Runs an app twice in one process: the second run must get every key, the first included.
//! Counts keys until `q`; prints both counts after the terminal is restored.

use ambit_tui::{App, Cmd, Event, KeyCode};
use ratatui_core::buffer::Buffer;
use ratatui_core::layout::Rect;
use ratatui_core::widgets::Widget;

struct Count(u32);

impl App for Count {
    type Msg = ();

    fn update(&mut self, event: Event<()>) -> Cmd<()> {
        match event {
            Event::Key(k) if k.code == KeyCode::Char('q') => Cmd::Quit,
            Event::Key(_) => {
                self.0 += 1;
                Cmd::None
            }
            _ => Cmd::None,
        }
    }

    fn view(&self, area: Rect, buf: &mut Buffer) {
        format!("keys: {} (q quits)", self.0).render(area, buf);
    }
}

fn main() -> std::io::Result<()> {
    let first = ambit_tui_native::run(Count(0), Default::default())?;
    let second = ambit_tui_native::run(Count(0), Default::default())?;
    println!("first {} second {}", first.0, second.0);
    Ok(())
}
