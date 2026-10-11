//! Example tui app: a todo list, stored in `Kv`.
//!
//! Keys: `a` add, `space` done/undone, `d` delete, `j`/`k` or arrows to move, `q` quit.
//! Storage runs as tasks, so the screen never waits on it. Status messages clear after 3 s.
#![forbid(
    unsafe_code,
    clippy::disallowed_methods,
    clippy::disallowed_types,
    clippy::print_stdout,
    clippy::print_stderr
)]

use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};

use ambit_caps::{Clock, Kv, KvError};
use ambit_tui::layout::{Constraint, Layout};
use ambit_tui::style::{Modifier, Style};
use ambit_tui::text::Line;
use ambit_tui::widgets::Widget;
use ambit_tui::{App, Buffer, Cmd, Event, Key, KeyCode, Rect};
use ratatui_widgets::paragraph::Paragraph;

const PREFIX: &[u8] = b"todo/";
const STATUS_FOR: Duration = Duration::from_secs(3);

pub fn init(clock: impl Clock, kv: impl Kv) -> impl App {
    Todo {
        clock: Box::new(clock),
        kv: Arc::new(kv),
        items: None,
        selected: 0,
        input: None,
        status: None,
    }
}

/// One todo. Public only because results carrying it travel through `Msg`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    id: u64,
    created: u64,
    done: bool,
    text: String,
}

impl Item {
    fn key(&self) -> Vec<u8> {
        key(self.id)
    }

    fn encode(&self) -> Vec<u8> {
        format!("{}\n{}\n{}", self.created, u8::from(self.done), self.text).into_bytes()
    }

    fn decode(key: &[u8], value: &[u8]) -> Option<Self> {
        let id = std::str::from_utf8(key.strip_prefix(PREFIX)?)
            .ok()?
            .parse()
            .ok()?;
        let mut parts = std::str::from_utf8(value).ok()?.splitn(3, '\n');
        Some(Self {
            id,
            created: parts.next()?.parse().ok()?,
            done: parts.next()? == "1",
            text: parts.next()?.to_string(),
        })
    }
}

/// Zero-padded, so keys sort in creation order.
fn key(id: u64) -> Vec<u8> {
    format!("todo/{id:020}").into_bytes()
}

pub enum Msg {
    Loaded(Result<Vec<Item>, KvError>),
    Saved(Result<(), KvError>),
    ClearStatus(u64),
}

struct Status {
    text: String,
    error: bool,
    seq: u64,
}

struct Todo<K> {
    clock: Box<dyn Clock>,
    kv: Arc<K>,
    /// `None` until loaded.
    items: Option<Vec<Item>>,
    selected: usize,
    /// The new item's text while adding.
    input: Option<String>,
    status: Option<Status>,
}

impl<K: Kv> Todo<K> {
    fn now(&self) -> u64 {
        self.clock
            .now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs())
    }

    fn save(&self, item: &Item) -> Cmd<Msg> {
        let (kv, key, value) = (self.kv.clone(), item.key(), item.encode());
        Cmd::task(move || Msg::Saved(kv.put(&key, &value)))
    }

    fn remove(&self, id: u64) -> Cmd<Msg> {
        let kv = self.kv.clone();
        Cmd::task(move || Msg::Saved(kv.delete(&key(id))))
    }

    fn show(&mut self, text: impl Into<String>, error: bool) -> Cmd<Msg> {
        let seq = self.status.as_ref().map_or(0, |s| s.seq + 1);
        self.status = Some(Status {
            text: text.into(),
            error,
            seq,
        });
        Cmd::After(STATUS_FOR, Msg::ClearStatus(seq))
    }

    fn key(&mut self, key: Key) -> Cmd<Msg> {
        if let Some(input) = &mut self.input {
            match key.code {
                KeyCode::Enter => {
                    let text = input.trim().to_string();
                    self.input = None;
                    return self.add(text);
                }
                KeyCode::Esc => self.input = None,
                KeyCode::Backspace => {
                    input.pop();
                }
                KeyCode::Char(c) if !key.mods.ctrl && !key.mods.alt => input.push(c),
                _ => {}
            }
            return Cmd::None;
        }
        if key == Key::ctrl('c') {
            return Cmd::Quit;
        }
        let Some(items) = &mut self.items else {
            return Cmd::None;
        };
        match key.code {
            KeyCode::Char('q') => return Cmd::Quit,
            KeyCode::Char('a') => self.input = Some(String::new()),
            KeyCode::Char('j') | KeyCode::Down => {
                self.selected = (self.selected + 1).min(items.len().saturating_sub(1));
            }
            KeyCode::Char('k') | KeyCode::Up => self.selected = self.selected.saturating_sub(1),
            KeyCode::Char(' ') => {
                if let Some(item) = items.get_mut(self.selected) {
                    item.done = !item.done;
                    let item = item.clone();
                    return self.save(&item);
                }
            }
            KeyCode::Char('d') if self.selected < items.len() => {
                let item = items.remove(self.selected);
                self.selected = self.selected.min(items.len().saturating_sub(1));
                return self.remove(item.id);
            }
            _ => {}
        }
        Cmd::None
    }

    fn add(&mut self, text: String) -> Cmd<Msg> {
        let created = self.now();
        let Some(items) = &mut self.items else {
            return Cmd::None;
        };
        if text.is_empty() {
            return Cmd::None;
        }
        let id = items.iter().map(|i| i.id + 1).max().unwrap_or(1);
        let item = Item {
            id,
            created,
            done: false,
            text,
        };
        items.push(item.clone());
        self.selected = items.len() - 1;
        self.save(&item)
    }
}

impl<K: Kv> App for Todo<K> {
    type Msg = Msg;

    fn start(&mut self) -> Cmd<Msg> {
        let kv = self.kv.clone();
        Cmd::task(move || {
            Msg::Loaded(kv.scan(PREFIX).map(|entries| {
                entries
                    .iter()
                    .filter_map(|(k, v)| Item::decode(k, v))
                    .collect()
            }))
        })
    }

    fn update(&mut self, event: Event<Msg>) -> Cmd<Msg> {
        match event {
            Event::Key(key) => self.key(key),
            Event::Paste(text) => {
                if let Some(input) = &mut self.input {
                    input.push_str(&text.replace(['\n', '\r'], " "));
                }
                Cmd::None
            }
            Event::Resize(..) => Cmd::None,
            Event::Msg(Msg::Loaded(Ok(items))) => {
                self.items = Some(items);
                Cmd::None
            }
            Event::Msg(Msg::Loaded(Err(e))) => {
                // Keep the list unloaded: editing would overwrite what storage still holds.
                self.status = Some(Status {
                    text: format!("can't load: {e}"),
                    error: true,
                    seq: 0,
                });
                Cmd::None
            }
            Event::Msg(Msg::Saved(Ok(()))) => self.show("saved", false),
            Event::Msg(Msg::Saved(Err(e))) => self.show(format!("not saved: {e}"), true),
            Event::Msg(Msg::ClearStatus(seq)) => {
                if self.status.as_ref().is_some_and(|s| s.seq == seq) {
                    self.status = None;
                }
                Cmd::None
            }
        }
    }

    fn view(&self, area: Rect, buf: &mut Buffer) {
        let [title, list, bottom] = Layout::vertical([
            Constraint::Length(2),
            Constraint::Min(0),
            Constraint::Length(1),
        ])
        .areas(area);

        let Some(items) = &self.items else {
            Line::from(" Todo").render(title, buf);
            let text = match &self.status {
                Some(s) => s.text.as_str(),
                None => "loading…",
            };
            Paragraph::new(format!(" {text}")).render(list, buf);
            return;
        };

        let open = items.iter().filter(|i| !i.done).count();
        Line::from(format!(" Todo  {open} open · {} done", items.len() - open))
            .style(Style::new().add_modifier(Modifier::BOLD))
            .render(title, buf);

        let now = self.now();
        let lines: Vec<Line> = if items.is_empty() {
            vec![Line::from(" Nothing to do. Press a to add.")]
        } else {
            items
                .iter()
                .enumerate()
                .map(|(i, item)| {
                    let mark = if item.done { "x" } else { " " };
                    let cursor = if i == self.selected { ">" } else { " " };
                    let age = age(now.saturating_sub(item.created));
                    let line = Line::from(format!("{cursor}[{mark}] {}  ({age})", item.text));
                    if item.done {
                        line.style(Style::new().add_modifier(Modifier::DIM))
                    } else {
                        line
                    }
                })
                .collect()
        };
        // Keep the selection visible on short screens.
        let scroll = (self.selected + 1).saturating_sub(list.height as usize);
        Paragraph::new(lines)
            .scroll((u16::try_from(scroll).unwrap_or(u16::MAX), 0))
            .render(list, buf);

        let text = match (&self.input, &self.status) {
            (Some(input), _) => format!(" New: {input}_"),
            (None, Some(status)) if status.error => format!(" ! {}", status.text),
            (None, Some(status)) => format!(" {}", status.text),
            (None, None) => " a add  space done  d delete  q quit".to_string(),
        };
        Line::from(text).render(bottom, buf);
    }
}

fn age(secs: u64) -> String {
    match secs {
        0..60 => "just now".to_string(),
        60..3600 => format!("{}m", secs / 60),
        3600..86_400 => format!("{}h", secs / 3600),
        _ => format!("{}d", secs / 86_400),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn items_round_trip_through_storage_format() {
        let item = Item {
            id: 7,
            created: 1_700_000_000,
            done: true,
            text: "milk\nand eggs".to_string(),
        };
        assert_eq!(Item::decode(&item.key(), &item.encode()), Some(item));
    }

    #[test]
    fn keys_sort_in_creation_order() {
        assert!(key(9) < key(10));
    }

    #[test]
    fn ages() {
        assert_eq!(age(59), "just now");
        assert_eq!(age(60 * 5), "5m");
        assert_eq!(age(3600 * 3), "3h");
        assert_eq!(age(86_400 * 2), "2d");
    }
}
