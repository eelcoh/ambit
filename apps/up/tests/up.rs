//! up, as a user sees it: addresses in, one line per address out.

use std::time::Duration;

use ambit_caps_net::{Http, HttpErrorKind, Origin, Scope, http};
use ambit_cli::{Args, Exit};
use ambit_cli_sim::{SimClock, SimHttp, args, run, run_on_terminal};

/// The web, as far as these tests go. Each answer takes 20 ms.
fn web(clock: &SimClock) -> SimHttp {
    let web = SimHttp::new(
        Scope::parse([
            "https://example.com",
            "http://old.example",
            "https://shop.example",
        ])
        .unwrap(),
    );
    web.respond("GET", "https://example.com/", 200, "hello");
    web.respond("GET", "https://shop.example/", 503, "");
    let moved = http::Response::builder()
        .status(301)
        .header("location", "https://example.com/")
        .body(Vec::new())
        .unwrap();
    web.handle(move |req| (req.uri() == "http://old.example/").then(|| Ok(moved.clone())));
    let clock = clock.clone();
    web.handle(move |_| {
        clock.advance(Duration::from_millis(20));
        None
    });
    web
}

/// What the wiring does: each address the user typed reaches its own site, and nothing else.
fn app(args: &Args, clock: &SimClock, web: &SimHttp) -> up::Up<SimClock, SimHttp> {
    let web = web.clone();
    up::init(
        clock.clone(),
        args.named(move |url| Ok(web.scoped(Scope::new([Origin::of(url)?])))),
    )
}

fn up(line: &[&str]) -> (ambit_cli_sim::Run<Exit>, SimHttp) {
    let clock = SimClock::at(std::time::UNIX_EPOCH);
    let web = web(&clock);
    let args = args(line);
    (run(app(&args, &clock, &web), &args, ""), web)
}

#[test]
fn each_address_gets_a_line() {
    let (run, _) = up(&["up", "https://example.com/", "http://old.example/"]);
    assert_eq!(run.done, Exit::SUCCESS, "{}", run.err);
    assert_eq!(
        run.out,
        "up\t200\t20\thttps://example.com/\t\n\
         moved\t301\t20\thttp://old.example/\tto https://example.com/\n"
    );
}

#[test]
fn lined_up_on_a_terminal() {
    let clock = SimClock::at(std::time::UNIX_EPOCH);
    let web = web(&clock);
    let args = args(&["up", "https://example.com/", "https://shop.example/"]);
    let shown = run_on_terminal(app(&args, &clock, &web), &args, "", 80);
    assert_eq!(shown.done, Exit::FAILURE);
    assert_eq!(
        shown.out,
        "up     200     20 ms  https://example.com/\n\
         down   503     20 ms  https://shop.example/  Service Unavailable\n"
    );
}

#[test]
fn no_answer_is_a_failure() {
    let clock = SimClock::at(std::time::UNIX_EPOCH);
    let web = web(&clock);
    web.fail(Some(HttpErrorKind::Timeout));
    let args = args(&["up", "https://example.com/"]);
    let run = run(app(&args, &clock, &web), &args, "");
    assert_eq!(run.done, Exit::FAILURE);
    assert!(run.out.starts_with("fail\t-\t"), "{}", run.out);
}

#[test]
fn not_an_address() {
    let (run, web) = up(&["up", "example.com"]);
    assert_eq!(run.done, Exit::FAILURE);
    assert_eq!(
        run.out,
        "fail\t-\t-\texample.com\texample.com: not a full URL, like https://example.com\n"
    );
    assert!(web.sent().is_empty());
}

/// A redirect is shown, not followed: the site it points to wasn't named, and the app couldn't
/// reach it even if it tried.
#[test]
fn only_the_named_sites_are_reached() {
    let (run, web) = up(&["up", "http://old.example/"]);
    assert_eq!(run.done, Exit::SUCCESS);
    let sent: Vec<String> = web.sent().into_iter().map(|s| s.uri).collect();
    assert_eq!(sent, ["http://old.example/"]);

    let args = args(&["up", "http://old.example/"]);
    let site = args
        .named({
            let web = web.clone();
            move |url| Ok(web.scoped(Scope::new([Origin::of(url)?])))
        })
        .open("http://old.example/")
        .unwrap();
    let follow = http::Request::get("https://example.com/")
        .body(Vec::new())
        .unwrap();
    assert_eq!(site.send(follow).unwrap_err().kind, HttpErrorKind::Denied);
}

#[test]
fn help_is_an_answer_and_no_address_is_a_usage_error() {
    let (help, _) = up(&["up", "--help"]);
    assert_eq!(help.done, Exit::SUCCESS);
    assert!(help.out.contains("Redirects are shown"), "{}", help.out);
    let (wrong, _) = up(&["up"]);
    assert_eq!(wrong.done, Exit::USAGE);
    assert!(wrong.err.contains("<URL>"), "{}", wrong.err);
}
