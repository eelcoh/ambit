//! Simulation backend for the tui platform. A test scripts events and reads the screen as text.
//!
//! Deterministic: tasks (`Task` and `Serial`) run inline, in the order they were asked for; timers
//! fire only when the test advances the `SimClock`, in deadline order. The first event is
//! `Resize` with the sim's size, as on a terminal. The same script always gives the same screen.
#![forbid(unsafe_code)]

use std::collections::VecDeque;
use std::time::Duration;

pub use ambit_caps_sim::{Faults, MemKv, SeededRandom, SimClock};
use ambit_tui::{App, Buffer, Cmd, Event, Key, Rect, flatten};

pub struct Sim<A: App> {
    app: A,
    area: Rect,
    clock: SimClock,
    /// Time since start, as the sim's timers see it. Moves with `advance`.
    now: Duration,
    timers: Vec<(Duration, u64, A::Msg)>,
    next_timer: u64,
    quit: bool,
}

impl<A: App> Sim<A> {
    /// Starts the app on a `width` x `height` screen. `clock` must be the clock the app got, so
    /// that `advance` moves the app's time and the timers together.
    pub fn new(mut app: A, clock: SimClock, width: u16, height: u16) -> Self {
        let start = app.start();
        let mut sim = Self {
            app,
            area: Rect::new(0, 0, width, height),
            clock,
            now: Duration::ZERO,
            timers: Vec::new(),
            next_timer: 0,
            quit: false,
        };
        // As on a terminal: the size is the first event, before any result of `start`'s tasks.
        let resized = sim.app.update(Event::Resize(width, height));
        sim.run(start);
        sim.run(resized);
        sim
    }

    /// A key press: `sim.key('a')`, `sim.key(KeyCode::Enter)`, `sim.key(Key::ctrl('c'))`.
    pub fn key(&mut self, key: impl Into<Key>) -> &mut Self {
        self.send(Event::Key(key.into()))
    }

    /// Types each character as a separate key press.
    pub fn keys(&mut self, text: &str) -> &mut Self {
        text.chars().for_each(|c| {
            self.key(c);
        });
        self
    }

    pub fn paste(&mut self, text: &str) -> &mut Self {
        self.send(Event::Paste(text.to_string()))
    }

    pub fn resize(&mut self, width: u16, height: u16) -> &mut Self {
        self.area = Rect::new(0, 0, width, height);
        self.send(Event::Resize(width, height))
    }

    /// Sends any event, then runs the commands it causes. Ignored after `Quit`, as on a terminal.
    pub fn send(&mut self, event: Event<A::Msg>) -> &mut Self {
        if !self.quit {
            let cmd = self.app.update(event);
            self.run(cmd);
        }
        self
    }

    /// Moves time forward, firing the timers that come due, earliest first, each at its own time.
    pub fn advance(&mut self, by: Duration) -> &mut Self {
        let end = self.now + by;
        while let Some(i) = self.due(end) {
            let (at, _, msg) = self.timers.remove(i);
            self.clock.advance(at - self.now);
            self.now = at;
            self.send(Event::Msg(msg));
        }
        self.clock.advance(end - self.now);
        self.now = end;
        self
    }

    fn due(&self, end: Duration) -> Option<usize> {
        let (i, (at, ..)) = self
            .timers
            .iter()
            .enumerate()
            .min_by_key(|(_, (at, seq, _))| (*at, *seq))?;
        (*at <= end).then_some(i)
    }

    fn run(&mut self, cmd: Cmd<A::Msg>) {
        let mut queue = VecDeque::new();
        let mut cmds = Vec::new();
        flatten(cmd, &mut cmds);
        queue.extend(cmds);
        while let Some(cmd) = queue.pop_front() {
            match cmd {
                Cmd::Quit => self.quit = true,
                Cmd::After(delay, msg) => {
                    self.timers.push((self.now + delay, self.next_timer, msg));
                    self.next_timer += 1;
                }
                Cmd::Task(work) | Cmd::Serial(work) => {
                    if self.quit {
                        continue;
                    }
                    let mut cmds = Vec::new();
                    flatten(self.app.update(Event::Msg(work())), &mut cmds);
                    queue.extend(cmds);
                }
                Cmd::None | Cmd::Batch(_) => unreachable!("flattened"),
            }
        }
    }

    /// Whether the app asked to quit.
    pub fn quit(&self) -> bool {
        self.quit
    }

    /// Timers that haven't fired yet.
    pub fn pending_timers(&self) -> usize {
        self.timers.len()
    }

    pub fn app(&self) -> &A {
        &self.app
    }

    /// The app, as native `run` gives it back when it ends.
    pub fn into_app(self) -> A {
        self.app
    }

    /// The screen as the user would see it: one line per row, trailing spaces trimmed.
    pub fn screen(&self) -> String {
        let buf = self.buffer();
        (0..self.area.height)
            .map(|y| {
                let line: String = (0..self.area.width).map(|x| buf[(x, y)].symbol()).collect();
                line.trim_end().to_string()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The rendered buffer, for assertions on style.
    pub fn buffer(&self) -> Buffer {
        let mut buf = Buffer::empty(self.area);
        self.app.view(self.area, &mut buf);
        buf
    }
}
