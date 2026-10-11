//! Acceptance properties of the todo example, on the tui sim backend.

use std::time::{Duration, UNIX_EPOCH};

use ambit_tui::{Key, KeyCode};
use ambit_tui_sim::{Faults, MemKv, Sim, SimClock};

fn start(kv: &MemKv) -> (Sim<impl ambit_tui::App>, SimClock) {
    let clock = SimClock::at(UNIX_EPOCH + Duration::from_secs(1_700_000_000));
    let sim = Sim::new(todo::init(clock.clone(), kv.clone()), clock.clone(), 50, 8);
    (sim, clock)
}

fn add(sim: &mut Sim<impl ambit_tui::App>, text: &str) {
    sim.key('a').keys(text).key(KeyCode::Enter);
}

#[test]
fn empty_list_explains_what_to_do() {
    let (sim, _) = start(&MemKv::new());
    assert!(
        sim.screen().contains("Nothing to do. Press a to add."),
        "{}",
        sim.screen()
    );
}

#[test]
fn added_items_show_and_survive_a_restart() {
    let kv = MemKv::new();
    let (mut sim, _) = start(&kv);
    add(&mut sim, "milk");
    add(&mut sim, "bread");
    let screen = sim.screen();
    assert!(screen.contains("2 open · 0 done"), "{screen}");
    assert!(screen.contains(" [ ] milk"), "{screen}");
    assert!(screen.contains(">[ ] bread"), "{screen}");

    let (restarted, _) = start(&kv);
    let screen = restarted.screen();
    assert!(screen.contains(">[ ] milk"), "{screen}");
    assert!(screen.contains(" [ ] bread"), "{screen}");
}

#[test]
fn the_screen_is_exactly_this() {
    let (mut sim, _) = start(&MemKv::new());
    add(&mut sim, "milk");
    sim.advance(Duration::from_secs(5));
    assert_eq!(
        sim.screen(),
        " Todo  1 open · 0 done\n\
         \n\
         >[ ] milk  (just now)\n\
         \n\
         \n\
         \n\
         \n\
         \x20a add  space done  d delete  q quit"
    );
}

#[test]
fn done_and_delete_are_stored() {
    let kv = MemKv::new();
    let (mut sim, _) = start(&kv);
    add(&mut sim, "milk");
    add(&mut sim, "bread");
    sim.key(KeyCode::Up).key(' ');
    assert!(sim.screen().contains(">[x] milk"), "{}", sim.screen());
    sim.key(KeyCode::Down).key('d');
    assert_eq!(kv.len(), 1);

    let (restarted, _) = start(&kv);
    assert!(
        restarted.screen().contains("0 open · 1 done"),
        "{}",
        restarted.screen()
    );
}

#[test]
fn escape_cancels_and_empty_text_adds_nothing() {
    let kv = MemKv::new();
    let (mut sim, _) = start(&kv);
    sim.key('a').keys("never").key(KeyCode::Esc);
    sim.key('a').keys("   ").key(KeyCode::Enter);
    assert!(kv.is_empty());
    assert!(sim.screen().contains("Nothing to do"));
}

#[test]
fn paste_goes_into_the_new_item_on_one_line() {
    let (mut sim, _) = start(&MemKv::new());
    sim.key('a').paste("two\nlines").key(KeyCode::Enter);
    assert!(sim.screen().contains("[ ] two lines"), "{}", sim.screen());
}

#[test]
fn status_clears_after_three_seconds() {
    let (mut sim, _) = start(&MemKv::new());
    add(&mut sim, "milk");
    assert!(sim.screen().ends_with(" saved"));
    sim.advance(Duration::from_millis(2999));
    assert!(sim.screen().ends_with(" saved"));
    sim.advance(Duration::from_millis(1));
    assert!(sim.screen().ends_with("q quit"));
}

#[test]
fn ages_follow_the_clock() {
    let (mut sim, _) = start(&MemKv::new());
    add(&mut sim, "milk");
    sim.advance(Duration::from_secs(3 * 3600));
    assert!(sim.screen().contains("milk  (3h)"), "{}", sim.screen());
}

#[test]
fn a_failed_save_is_shown() {
    let kv = MemKv::new();
    let (mut sim, _) = start(&kv);
    kv.fail(Faults::Writes);
    add(&mut sim, "milk");
    assert!(
        sim.screen()
            .contains("! not saved: kv: injected write failure"),
        "{}",
        sim.screen()
    );
}

#[test]
fn storage_down_at_start_blocks_editing() {
    let kv = MemKv::new();
    kv.fail(Faults::All);
    let (mut sim, _) = start(&kv);
    assert!(sim.screen().contains("can't load"), "{}", sim.screen());
    add(&mut sim, "milk");
    kv.fail(Faults::None);
    assert!(kv.is_empty());
}

#[test]
fn quits_on_q_and_ctrl_c_but_q_is_text_while_adding() {
    let (mut sim, _) = start(&MemKv::new());
    sim.key('a').key('q');
    assert!(!sim.quit());
    sim.key(KeyCode::Enter).key('q');
    assert!(sim.quit());

    let (mut sim, _) = start(&MemKv::new());
    sim.key(Key::ctrl('c'));
    assert!(sim.quit());
}

#[test]
fn the_selection_stays_visible_on_a_short_screen() {
    let (mut sim, _) = start(&MemKv::new());
    for i in 0..10 {
        add(&mut sim, &format!("item {i}"));
    }
    assert!(sim.screen().contains(">[ ] item 9"), "{}", sim.screen());
}
