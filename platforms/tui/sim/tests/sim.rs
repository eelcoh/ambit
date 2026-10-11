//! The sim backend's own guarantees: ordering of tasks and timers, and time as the app sees it.

use std::time::{Duration, UNIX_EPOCH};

use ambit_caps::Clock;
use ambit_tui::{App, Buffer, Cmd, Event, Rect};
use ambit_tui_sim::{Sim, SimClock};

/// Records every message with the clock reading at the time it arrived.
struct Recorder {
    clock: SimClock,
    seen: Vec<(&'static str, Duration)>,
    start: Option<Cmd<&'static str>>,
    on: fn(&'static str) -> Cmd<&'static str>,
}

impl App for Recorder {
    type Msg = &'static str;
    fn start(&mut self) -> Cmd<&'static str> {
        self.start.take().unwrap_or(Cmd::None)
    }
    fn update(&mut self, event: Event<&'static str>) -> Cmd<&'static str> {
        match event {
            Event::Msg(m) => {
                self.seen.push((m, self.clock.elapsed()));
                (self.on)(m)
            }
            _ => Cmd::Quit,
        }
    }
    fn view(&self, _: Rect, _: &mut Buffer) {}
}

fn sim(start: Cmd<&'static str>, on: fn(&'static str) -> Cmd<&'static str>) -> Sim<Recorder> {
    let clock = SimClock::at(UNIX_EPOCH);
    let app = Recorder {
        clock: clock.clone(),
        seen: Vec::new(),
        start: Some(start),
        on,
    };
    Sim::new(app, clock, 10, 2)
}

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

#[test]
fn timers_fire_in_deadline_order_at_their_own_time() {
    let mut sim = sim(
        Cmd::Batch(vec![
            Cmd::After(ms(30), "c"),
            Cmd::After(ms(10), "a"),
            Cmd::After(ms(10), "b"),
        ]),
        |_| Cmd::None,
    );
    sim.advance(ms(20));
    assert_eq!(sim.app().seen, [("a", ms(10)), ("b", ms(10))]);
    assert_eq!(sim.pending_timers(), 1);
    sim.advance(ms(100));
    assert_eq!(sim.app().seen.last(), Some(&("c", ms(30))));
}

#[test]
fn a_timer_set_by_a_timer_counts_from_when_it_fired() {
    let mut sim = sim(Cmd::After(ms(10), "tick"), |m| match m {
        "tick" => Cmd::After(ms(10), "tock"),
        _ => Cmd::None,
    });
    sim.advance(ms(25));
    assert_eq!(sim.app().seen, [("tick", ms(10)), ("tock", ms(20))]);
}

#[test]
fn tasks_run_inline_in_order_and_their_commands_follow() {
    let sim = sim(
        Cmd::Batch(vec![Cmd::task(|| "one"), Cmd::task(|| "two")]),
        |m| match m {
            "one" => Cmd::task(|| "three"),
            _ => Cmd::None,
        },
    );
    let order: Vec<_> = sim.app().seen.iter().map(|(m, _)| *m).collect();
    assert_eq!(order, ["one", "two", "three"]);
}

#[test]
fn nothing_runs_after_quit() {
    let mut sim = sim(Cmd::Batch(vec![Cmd::Quit, Cmd::task(|| "late")]), |_| {
        Cmd::None
    });
    assert!(sim.quit());
    sim.key('x');
    assert!(sim.app().seen.is_empty());
}
