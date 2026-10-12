//! The tui platform: what a terminal app is written against.
//!
//! The platform owns the terminal, the event loop and concurrency. The app gets events, updates
//! its model and returns commands; it draws into a `Buffer` and never touches the terminal.
#![forbid(unsafe_code)]

use std::time::Duration;

/// The view vocabulary: ratatui's buffer and layout types. Apps draw with `ratatui-widgets`.
pub use ratatui_core::{buffer::Buffer, layout::Rect};
pub use ratatui_core::{layout, style, text, widgets};

/// What happened, in the order it happened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event<M> {
    /// A key press. Releases and repeats are not reported.
    Key(Key),
    /// Text pasted into the terminal, in one piece.
    Paste(String),
    /// The terminal size (columns, rows). Always the first event, right after `start`, and again
    /// whenever the size changes. The platform redraws after every event.
    Resize(u16, u16),
    /// A message the app asked for: the result of a `Task` or an `After` timer.
    Msg(M),
}

/// A key press.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Key {
    pub code: KeyCode,
    pub mods: Mods,
}

impl Key {
    pub const fn new(code: KeyCode) -> Self {
        Self {
            code,
            mods: Mods::NONE,
        }
    }

    pub const fn ctrl(c: char) -> Self {
        Self {
            code: KeyCode::Char(c),
            mods: Mods::CTRL,
        }
    }
}

impl From<KeyCode> for Key {
    fn from(code: KeyCode) -> Self {
        Self::new(code)
    }
}

impl From<char> for Key {
    fn from(c: char) -> Self {
        Self::new(KeyCode::Char(c))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KeyCode {
    /// A character, with shift already applied (`'A'`, not `'a'` + shift).
    Char(char),
    Enter,
    Esc,
    Backspace,
    Delete,
    Tab,
    BackTab,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    Insert,
    F(u8),
}

/// Modifier keys held during a key press.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Mods {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
}

impl Mods {
    pub const NONE: Self = Self {
        ctrl: false,
        alt: false,
        shift: false,
    };
    pub const CTRL: Self = Self {
        ctrl: true,
        alt: false,
        shift: false,
    };
}

/// Blocking work the platform runs off the UI thread. It captures the capabilities it needs,
/// and can report progress before it returns its result.
pub type Task<M> = Box<dyn FnOnce(&Progress<M>) -> M + Send>;

/// Hands messages from a running task to the app before the task's result: the phases of a
/// long transfer, bytes so far. They arrive in the order sent, all before the result.
pub struct Progress<M>(Box<dyn Fn(M) + Send>);

impl<M> Progress<M> {
    /// For platforms: `deliver` gets each message.
    pub fn new(deliver: impl Fn(M) + Send + 'static) -> Self {
        Self(Box::new(deliver))
    }

    pub fn send(&self, msg: M) {
        (self.0)(msg)
    }
}

/// What the app asks the platform to do after an update.
pub enum Cmd<M> {
    None,
    /// End the program. The platform restores the terminal.
    Quit,
    Batch(Vec<Cmd<M>>),
    /// Deliver `M` after this long, measured on the platform's clock.
    After(Duration, M),
    /// Run this on the platform's worker pool and deliver its result. Tasks run concurrently:
    /// results can arrive in any order.
    Task(Task<M>),
    /// Run this after every earlier `Serial` task has finished, one at a time, in the order
    /// asked. For effects that must not overlap (one device, one session). Still off the UI
    /// thread; `Task`s keep running alongside.
    Serial(Task<M>),
}

impl<M> Cmd<M> {
    pub fn task(work: impl FnOnce() -> M + Send + 'static) -> Self {
        Self::Task(Box::new(move |_| work()))
    }

    pub fn serial(work: impl FnOnce() -> M + Send + 'static) -> Self {
        Self::Serial(Box::new(move |_| work()))
    }

    /// Like [`Cmd::task`], for work that reports progress on the way.
    pub fn task_with_progress(work: impl FnOnce(&Progress<M>) -> M + Send + 'static) -> Self {
        Self::Task(Box::new(work))
    }

    /// Like [`Cmd::serial`], for work that reports progress on the way.
    pub fn serial_with_progress(work: impl FnOnce(&Progress<M>) -> M + Send + 'static) -> Self {
        Self::Serial(Box::new(work))
    }
}

/// A terminal app. The platform calls `update` for every event, then `view` to redraw.
pub trait App: 'static {
    type Msg: Send + 'static;

    /// Commands to run at startup, before the first event (e.g. a first timer).
    fn start(&mut self) -> Cmd<Self::Msg> {
        Cmd::None
    }

    fn update(&mut self, event: Event<Self::Msg>) -> Cmd<Self::Msg>;

    /// Draws the whole screen. Must not change the model.
    fn view(&self, area: Rect, buf: &mut Buffer);
}

/// Flattens a command tree into the order the platform runs it.
pub fn flatten<M>(cmd: Cmd<M>, out: &mut Vec<Cmd<M>>) {
    match cmd {
        Cmd::None => {}
        Cmd::Batch(cmds) => cmds.into_iter().for_each(|c| flatten(c, out)),
        other => out.push(other),
    }
}
