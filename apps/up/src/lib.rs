//! Example cli app: checks whether web addresses answer, and how fast. Agent-owned.
//!
//! ```text
//! up URL...   one line per address: up (2xx), moved (3xx), down (4xx, 5xx) or fail (no answer)
//! ```
//!
//! It reaches only the sites of the addresses named on the command line. A redirect is shown,
//! not followed: following it could lead to a site that wasn't named.

use ambit_caps::Clock;
use ambit_caps_net::{Http, http};
use ambit_cli::{App, Exit, Io, Named};
use clap::{Arg, Command};

pub fn init<C: Clock, H: Http>(clock: C, sites: Named<H>) -> Up<C, H> {
    Up { clock, sites }
}

pub struct Up<C, H> {
    clock: C,
    sites: Named<H>,
}

/// The command line, with clap's builder (see the keep example for why not the derive).
fn command() -> Command {
    Command::new("up")
        .version(env!("CARGO_PKG_VERSION"))
        .about(
            "Check whether web addresses answer. Exits with 1 when one is down or doesn't \
             answer. Redirects are shown, not followed.",
        )
        .arg(
            Arg::new("urls")
                .value_name("URL")
                .required(true)
                .num_args(1..)
                .help("A full address, like https://example.com/status"),
        )
}

/// What one address answered.
struct Line {
    state: &'static str,
    status: String,
    ms: String,
    url: String,
    note: String,
}

impl<C: Clock, H: Http> App for Up<C, H> {
    type Done = Exit;

    fn run(self, args: &[String], io: &mut Io<'_>) -> Exit {
        let matches = match command().try_get_matches_from(args) {
            Ok(matches) => matches,
            Err(e) if e.use_stderr() => {
                let _ = write!(io.err, "{}", e.render());
                return Exit::USAGE;
            }
            Err(e) => {
                let _ = write!(io.out, "{}", e.render());
                return Exit::SUCCESS;
            }
        };
        let urls: Vec<String> = matches
            .get_many::<String>("urls")
            .into_iter()
            .flatten()
            .cloned()
            .collect();
        let mut all_fine = true;
        for url in &urls {
            let line = self.check(url);
            all_fine &= matches!(line.state, "up" | "moved");
            if self.show(&line, io).is_err() {
                return Exit::FAILURE;
            }
        }
        if all_fine {
            Exit::SUCCESS
        } else {
            Exit::FAILURE
        }
    }
}

impl<C: Clock, H: Http> Up<C, H> {
    fn check(&self, url: &str) -> Line {
        let fail = |note: String| Line {
            state: "fail",
            status: "-".into(),
            ms: "-".into(),
            url: url.to_owned(),
            note,
        };
        let site = match self.sites.open(url) {
            Ok(site) => site,
            Err(e) => return fail(e.to_string()),
        };
        let request = match http::Request::get(url)
            .header(
                http::header::USER_AGENT,
                concat!("up/", env!("CARGO_PKG_VERSION")),
            )
            .body(Vec::new())
        {
            Ok(request) => request,
            Err(e) => return fail(format!("{url}: {e}")),
        };
        let start = self.clock.elapsed();
        let result = site.send(request);
        let ms = self.clock.elapsed().saturating_sub(start).as_millis();
        let response = match result {
            Ok(response) => response,
            Err(e) => {
                return Line {
                    ms: ms.to_string(),
                    ..fail(e.to_string())
                };
            }
        };
        let status = response.status();
        let (state, note) = if status.is_success() {
            ("up", String::new())
        } else if status.is_redirection() {
            let to = response
                .headers()
                .get(http::header::LOCATION)
                .and_then(|l| l.to_str().ok())
                .unwrap_or("nowhere given");
            ("moved", format!("to {to}"))
        } else {
            ("down", status.canonical_reason().unwrap_or("").to_owned())
        };
        Line {
            state,
            status: status.as_u16().to_string(),
            ms: ms.to_string(),
            url: url.to_owned(),
            note,
        }
    }

    fn show(&self, line: &Line, io: &mut Io<'_>) -> std::io::Result<()> {
        let Line {
            state,
            status,
            ms,
            url,
            note,
        } = line;
        if io.terminal.is_some() {
            // Lined up for reading; a pipe gets tabs, for the next program.
            let ms = if ms == "-" {
                "-".into()
            } else {
                format!("{ms} ms")
            };
            let line = format!("{state:<5}  {status:>3}  {ms:>8}  {url}  {note}");
            writeln!(io.out, "{}", line.trim_end())
        } else {
            writeln!(io.out, "{state}\t{status}\t{ms}\t{url}\t{note}")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_command_line_is_well_formed() {
        command().debug_assert();
    }
}
