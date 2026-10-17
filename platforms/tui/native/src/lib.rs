//! Native terminal backend for the tui platform: crossterm on Linux/Mac.
//!
//! Owns the terminal: raw mode, the alternate screen and bracketed paste, restored on return,
//! on error and on panic. Runs tasks on a fixed pool of worker threads, serial tasks on one more
//! thread of their own, and timers on the monotonic clock.
#![forbid(unsafe_code)]

use std::fs::File;
use std::io::{self, Stdout};
use std::panic::{self, AssertUnwindSafe};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, Once};
use std::thread;
use std::time::{Duration, Instant};

pub use ambit_caps_native::{MemKv, OsRandom, StdClock};
use ambit_tui::{App, Cmd, Event, Key, KeyCode, Mods, Progress, Task, flatten};
use ratatui_core::terminal::Terminal;
use ratatui_crossterm::CrosstermBackend;
use ratatui_crossterm::crossterm::{cursor, event, execute, terminal};

/// Sends tracing output to a file: the terminal belongs to the app's screen.
pub fn init_tracing(path: impl AsRef<Path>) -> io::Result<()> {
    let file = File::create(path)?;
    tracing_subscriber::fmt()
        .with_ansi(false)
        .with_writer(Mutex::new(file))
        .init();
    Ok(())
}

pub struct Options {
    /// Worker threads for `Cmd::Task`. Tasks beyond this wait their turn.
    pub workers: usize,
}

impl Default for Options {
    fn default() -> Self {
        Self { workers: 4 }
    }
}

/// Runs the app until it returns `Cmd::Quit`, and gives it back: the wiring can ask it what
/// it ended with (new settings, say) and run it, or a new one, again. By then the terminal is
/// restored and the input reader has stopped. Tasks still running finish on their own; their
/// results are dropped.
pub fn run<A: App>(app: A, options: Options) -> io::Result<A> {
    let _restore = Restore::enter()?;
    let mut term = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    let mut lp = Loop::new(app, options);
    lp.run(&mut term)?;
    Ok(lp.finish())
}

enum Input<M> {
    Term(event::Event),
    Msg(M),
    Failed(io::Error),
}

struct Loop<A: App> {
    app: A,
    /// Keeps the channel open even if the input thread and all workers have ended.
    _tx: Sender<Input<A::Msg>>,
    rx: Receiver<Input<A::Msg>>,
    jobs: Sender<Task<A::Msg>>,
    serial: Sender<Task<A::Msg>>,
    timers: Vec<(Instant, u64, A::Msg)>,
    next_timer: u64,
    _reader: Reader,
}

/// The input reader thread; stopped and joined on drop, also on an error return, so no reader
/// is left behind.
struct Reader {
    stop: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
}

impl Drop for Reader {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// How often the input reader checks whether it should stop.
const INPUT_POLL: Duration = Duration::from_millis(50);

impl<A: App> Loop<A> {
    fn new(app: A, options: Options) -> Self {
        let (tx, rx) = mpsc::channel();
        let (jobs, queue) = mpsc::channel::<Task<A::Msg>>();
        let queue = Arc::new(Mutex::new(queue));
        for _ in 0..options.workers.max(1) {
            let (queue, tx) = (queue.clone(), tx.clone());
            thread::spawn(move || worker(&queue, &tx));
        }
        // One worker on its own queue: serial tasks run one at a time, in order.
        let (serial, serial_queue) = mpsc::channel::<Task<A::Msg>>();
        let serial_queue = Arc::new(Mutex::new(serial_queue));
        let serial_tx = tx.clone();
        thread::spawn(move || worker(&serial_queue, &serial_tx));
        let input = tx.clone();
        let stop_input = Arc::new(AtomicBool::new(false));
        let stop = stop_input.clone();
        // Polls instead of blocking in read, so it can stop when the loop ends: a reader left
        // blocked would take the first key of the next run.
        let reader = thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                let sent = match event::poll(INPUT_POLL) {
                    Ok(false) => continue,
                    Ok(true) => match event::read() {
                        Ok(e) => input.send(Input::Term(e)),
                        Err(e) => input.send(Input::Failed(e)),
                    },
                    Err(e) => input.send(Input::Failed(e)),
                };
                if sent.is_err() {
                    break;
                }
            }
        });
        Self {
            app,
            _tx: tx,
            rx,
            jobs,
            serial,
            timers: Vec::new(),
            next_timer: 0,
            _reader: Reader {
                stop: stop_input,
                thread: Some(reader),
            },
        }
    }

    /// Stops the input reader and gives the app back. The task queues close too: workers end
    /// after their current task.
    fn finish(self) -> A {
        self.app
    }

    fn run(&mut self, term: &mut Terminal<CrosstermBackend<Stdout>>) -> io::Result<()> {
        let start = self.app.start();
        if self.exec(start) {
            return Ok(());
        }
        let size = term.size()?;
        let cmd = self.app.update(Event::Resize(size.width, size.height));
        if self.exec(cmd) {
            return Ok(());
        }
        loop {
            term.draw(|frame| {
                let area = frame.area();
                self.app.view(area, frame.buffer_mut())
            })?;
            let timeout = self
                .timers
                .iter()
                .map(|(at, ..)| at.saturating_duration_since(Instant::now()))
                .min()
                .unwrap_or(Duration::from_secs(3600));
            let mut inputs = match self.rx.recv_timeout(timeout) {
                Ok(input) => vec![input],
                Err(RecvTimeoutError::Timeout) => Vec::new(),
                Err(RecvTimeoutError::Disconnected) => unreachable!("the loop holds a sender"),
            };
            inputs.extend(self.rx.try_iter());
            for input in inputs {
                let event = match input {
                    Input::Term(e) => match convert(e) {
                        Some(event) => event,
                        None => continue,
                    },
                    Input::Msg(msg) => Event::Msg(msg),
                    Input::Failed(e) => return Err(e),
                };
                let cmd = self.app.update(event);
                if self.exec(cmd) {
                    return Ok(());
                }
            }
            if self.fire_due() {
                return Ok(());
            }
        }
    }

    /// Fires the timers that are due, earliest first. True if the app quit.
    fn fire_due(&mut self) -> bool {
        loop {
            let now = Instant::now();
            let due = self
                .timers
                .iter()
                .enumerate()
                .filter(|(_, (at, ..))| *at <= now)
                .min_by_key(|(_, (at, seq, _))| (*at, *seq))
                .map(|(i, _)| i);
            let Some(i) = due else { return false };
            let (_, _, msg) = self.timers.remove(i);
            let cmd = self.app.update(Event::Msg(msg));
            if self.exec(cmd) {
                return true;
            }
        }
    }

    /// Runs commands. True if the app quit.
    fn exec(&mut self, cmd: Cmd<A::Msg>) -> bool {
        let mut cmds = Vec::new();
        flatten(cmd, &mut cmds);
        for cmd in cmds {
            match cmd {
                Cmd::Quit => return true,
                Cmd::After(delay, msg) => {
                    self.timers
                        .push((Instant::now() + delay, self.next_timer, msg));
                    self.next_timer += 1;
                }
                Cmd::Task(work) => {
                    self.jobs.send(work).expect("workers outlive the loop");
                }
                Cmd::Serial(work) => {
                    self.serial.send(work).expect("workers outlive the loop");
                }
                Cmd::None | Cmd::Batch(_) => unreachable!("flattened"),
            }
        }
        false
    }
}

fn worker<M: Send + 'static>(queue: &Mutex<Receiver<Task<M>>>, tx: &Sender<Input<M>>) {
    loop {
        let job = match queue.lock() {
            Ok(rx) => rx.recv(),
            Err(_) => return,
        };
        let Ok(job) = job else { return };
        // The same channel as the result, from the same thread: progress arrives first, in order.
        let progress_tx = tx.clone();
        let progress = Progress::new(move |msg| {
            let _ = progress_tx.send(Input::Msg(msg));
        });
        let result = match panic::catch_unwind(AssertUnwindSafe(|| job(&progress))) {
            Ok(msg) => Input::Msg(msg),
            Err(_) => Input::Failed(io::Error::other("a task panicked")),
        };
        if tx.send(result).is_err() {
            return;
        }
    }
}

fn convert<M>(e: event::Event) -> Option<Event<M>> {
    use event::{Event as E, KeyCode as C, KeyEventKind, KeyModifiers as KM};
    match e {
        E::Key(k) if k.kind == KeyEventKind::Press => {
            let code = match k.code {
                C::Char(c) => KeyCode::Char(c),
                C::Enter => KeyCode::Enter,
                C::Esc => KeyCode::Esc,
                C::Backspace => KeyCode::Backspace,
                C::Delete => KeyCode::Delete,
                C::Tab => KeyCode::Tab,
                C::BackTab => KeyCode::BackTab,
                C::Up => KeyCode::Up,
                C::Down => KeyCode::Down,
                C::Left => KeyCode::Left,
                C::Right => KeyCode::Right,
                C::Home => KeyCode::Home,
                C::End => KeyCode::End,
                C::PageUp => KeyCode::PageUp,
                C::PageDown => KeyCode::PageDown,
                C::Insert => KeyCode::Insert,
                C::F(n) => KeyCode::F(n),
                _ => return None,
            };
            let mods = Mods {
                ctrl: k.modifiers.contains(KM::CONTROL),
                alt: k.modifiers.contains(KM::ALT),
                // Characters arrive with shift applied; reporting it too would make 'A' != 'A'.
                shift: k.modifiers.contains(KM::SHIFT) && !matches!(code, KeyCode::Char(_)),
            };
            Some(Event::Key(Key { code, mods }))
        }
        E::Paste(text) => Some(Event::Paste(text)),
        E::Resize(w, h) => Some(Event::Resize(w, h)),
        _ => None,
    }
}

/// Puts the terminal in app mode, and back on drop and on panic.
struct Restore;

/// Whether the terminal is in app mode, for the panic hook.
static ACTIVE: AtomicBool = AtomicBool::new(false);

impl Restore {
    fn enter() -> io::Result<Self> {
        // Installed once, however often `run` is called; restores only while a run is active.
        static HOOK: Once = Once::new();
        HOOK.call_once(|| {
            let previous = panic::take_hook();
            panic::set_hook(Box::new(move |info| {
                if ACTIVE.load(Ordering::Relaxed) {
                    restore();
                }
                previous(info);
            }));
        });
        ACTIVE.store(true, Ordering::Relaxed);
        terminal::enable_raw_mode()?;
        let guard = Self;
        execute!(
            io::stdout(),
            terminal::EnterAlternateScreen,
            event::EnableBracketedPaste,
            cursor::Hide
        )?;
        Ok(guard)
    }
}

impl Drop for Restore {
    fn drop(&mut self) {
        restore();
        ACTIVE.store(false, Ordering::Relaxed);
    }
}

fn restore() {
    let _ = execute!(
        io::stdout(),
        event::DisableBracketedPaste,
        terminal::LeaveAlternateScreen,
        cursor::Show
    );
    let _ = terminal::disable_raw_mode();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_arrives_in_order_before_the_result() {
        let (jobs, queue) = mpsc::channel::<Task<u32>>();
        let (tx, rx) = mpsc::channel();
        let queue = Mutex::new(queue);
        jobs.send(Box::new(|p: &Progress<u32>| {
            for i in 0..100 {
                p.send(i);
            }
            100
        }))
        .unwrap();
        drop(jobs);
        worker(&queue, &tx);
        let got: Vec<u32> = rx
            .try_iter()
            .map(|input| match input {
                Input::Msg(m) => m,
                _ => panic!("expected messages only"),
            })
            .collect();
        assert_eq!(got, (0..=100).collect::<Vec<_>>());
    }
}
