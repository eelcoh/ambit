//! Example cli app: keeps copies of files in a folder, one subfolder per day, and never
//! overwrites anything. Agent-owned.
//!
//! ```text
//! keep add FILE... --into FOLDER   copies each file into FOLDER/<today>/
//! keep list FOLDER                 what is kept, day by day
//! ```
//!
//! It opens only the files and folders named on the command line, and can only add to a folder.

use std::io::{Read, Write};

use ambit_caps::Clock;
use ambit_caps_dir::{Dir, DirErrorKind, DirWrite, OneFile, RelPath, STAGING_PREFIX};
use ambit_cli::{App, Exit, Io, Named};
use clap::{Arg, ArgMatches, Command};

/// The largest file kept: it's read into memory whole.
const MAX_FILE: u64 = 64 << 20;

pub fn init<C: Clock, D: DirWrite>(
    clock: C,
    files: Named<OneFile>,
    folders: Named<D>,
) -> Keep<C, D> {
    Keep {
        clock,
        files,
        folders,
    }
}

pub struct Keep<C, D> {
    clock: C,
    files: Named<OneFile>,
    folders: Named<D>,
}

/// The command line. Described with clap's builder: its derive macros switch off whole lint groups
/// in the code they generate, which the app rules forbid.
fn command() -> Command {
    Command::new("keep")
        .version(env!("CARGO_PKG_VERSION"))
        .about("Keep copies of files, one folder per day, never overwriting")
        .subcommand_required(true)
        .subcommand(
            Command::new("add")
                .about(
                    "Copy files into FOLDER/<today>/. A different file with a name already taken \
                     gets a number; the same file again is left as it is.",
                )
                .arg(Arg::new("files").required(true).num_args(1..))
                .arg(
                    Arg::new("into")
                        .long("into")
                        .value_name("FOLDER")
                        .required(true)
                        .help("The folder to keep them in. Created if it isn't there."),
                ),
        )
        .subcommand(
            Command::new("list")
                .about("List what is kept, day by day.")
                .arg(Arg::new("folder").required(true)),
        )
}

fn values(matches: &ArgMatches, id: &str) -> Vec<String> {
    matches
        .get_many::<String>(id)
        .into_iter()
        .flatten()
        .cloned()
        .collect()
}

fn value(matches: &ArgMatches, id: &str) -> String {
    values(matches, id).pop().unwrap_or_default()
}

impl<C: Clock, D: DirWrite> App for Keep<C, D> {
    type Done = Exit;

    fn run(self, args: &[String], io: &mut Io<'_>) -> Exit {
        let matches = match command().try_get_matches_from(args) {
            Ok(matches) => matches,
            // Help and --version are answers, to stdout; a wrong command line is an error.
            Err(e) if e.use_stderr() => {
                let _ = write!(io.err, "{}", e.render());
                return Exit::USAGE;
            }
            Err(e) => {
                let _ = write!(io.out, "{}", e.render());
                return Exit::SUCCESS;
            }
        };
        let result = match matches.subcommand() {
            Some(("add", m)) => self.add(&values(m, "files"), &value(m, "into"), io),
            Some(("list", m)) => self.list(&value(m, "folder"), io),
            _ => unreachable!("a subcommand is required"),
        };
        match result {
            Ok(exit) => exit,
            Err(e) => {
                let _ = writeln!(io.err, "keep: {e}");
                Exit::FAILURE
            }
        }
    }
}

type Result<T> = std::result::Result<T, String>;

impl<C: Clock, D: DirWrite> Keep<C, D> {
    fn add(&self, files: &[String], into: &str, io: &mut Io<'_>) -> Result<Exit> {
        let folder = self.folders.open(into).map_err(|e| e.to_string())?;
        let day = RelPath::new([date(&self.clock)]).map_err(|e| e.to_string())?;
        folder
            .create_dir(&day)
            .map_err(|e| format!("{into}: {e}"))?;
        let mut failed = 0;
        for name in files {
            match self.keep(name, &folder, &day) {
                Ok((kept, true)) => writeln!(io.out, "kept     {name} as {kept}"),
                Ok((kept, false)) => writeln!(io.out, "already  {name} as {kept}"),
                Err(e) => {
                    failed += 1;
                    writeln!(io.err, "keep: {name}: {e}")
                }
            }
            .map_err(|e| e.to_string())?;
        }
        Ok(if failed == 0 {
            Exit::SUCCESS
        } else {
            Exit::FAILURE
        })
    }

    /// Keeps one file in `day`: where it went, and whether it's new there.
    fn keep(&self, name: &str, folder: &D, day: &RelPath) -> Result<(RelPath, bool)> {
        let file = self.files.open(name).map_err(|e| e.to_string())?;
        let contents = read(&file, &file.path())?;
        for attempt in 1..1000 {
            let target = day
                .join(&numbered(file.name(), attempt))
                .map_err(|e| e.to_string())?;
            match folder.create_new(&target, &contents) {
                Ok(()) => return Ok((target, true)),
                Err(e) if e.kind == DirErrorKind::Exists => {
                    if read(folder, &target).is_ok_and(|kept| kept == contents) {
                        return Ok((target, false));
                    }
                }
                Err(e) => return Err(e.to_string()),
            }
        }
        Err("too many files with this name today".into())
    }

    fn list(&self, name: &str, io: &mut Io<'_>) -> Result<Exit> {
        let folder = self.folders.open(name).map_err(|e| e.to_string())?;
        let mut rows = Vec::new();
        for day in folder.list(&RelPath::root()).map_err(|e| e.to_string())? {
            if !day.is_dir {
                continue;
            }
            let path = RelPath::new([day.name.as_str()]).map_err(|e| e.to_string())?;
            for entry in folder.list(&path).map_err(|e| e.to_string())? {
                if entry.is_dir || entry.name.starts_with(STAGING_PREFIX) {
                    continue;
                }
                let len = path
                    .join(&entry.name)
                    .and_then(|p| folder.metadata(&p))
                    .map_or(0, |m| m.len);
                rows.push([day.name.clone(), entry.name, len.to_string()]);
            }
        }
        let out = &mut io.out;
        let result = if rows.is_empty() {
            writeln!(io.err, "Nothing kept in {name} yet.")
        } else if io.terminal.is_some() {
            // Lined up for reading; a pipe gets tabs, for the next program.
            let width = rows.iter().map(|r| r[1].chars().count()).max().unwrap_or(0);
            rows.iter()
                .try_for_each(|[day, file, len]| writeln!(out, "{day}  {file:<width$}  {len:>10}"))
        } else {
            rows.iter()
                .try_for_each(|row| writeln!(out, "{}", row.join("\t")))
        };
        result.map_err(|e| e.to_string())?;
        Ok(Exit::SUCCESS)
    }
}

fn read(dir: &impl Dir, path: &RelPath) -> Result<Vec<u8>> {
    let meta = dir.metadata(path).map_err(|e| e.to_string())?;
    if meta.is_dir {
        return Err("a folder, not a file".into());
    }
    if meta.len > MAX_FILE {
        return Err(format!("larger than {} MiB", MAX_FILE >> 20));
    }
    let mut contents = Vec::new();
    dir.open(path)
        .map_err(|e| e.to_string())?
        .take(MAX_FILE + 1)
        .read_to_end(&mut contents)
        .map_err(|e| e.to_string())?;
    Ok(contents)
}

/// `report.pdf`, then `report (2).pdf`, `report (3).pdf`, ...
fn numbered(name: &str, attempt: u32) -> String {
    if attempt == 1 {
        return name.to_owned();
    }
    match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => format!("{stem} ({attempt}).{ext}"),
        _ => format!("{name} ({attempt})"),
    }
}

/// Today in UTC, as `2026-10-13`.
fn date(clock: &impl Clock) -> String {
    let secs = clock
        .now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    // Days to a civil date (Howard Hinnant's algorithm).
    let z = (secs / 86_400) as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_command_line_is_well_formed() {
        command().debug_assert();
    }

    #[test]
    fn names_get_numbers_before_the_extension() {
        assert_eq!(numbered("a.pdf", 1), "a.pdf");
        assert_eq!(numbered("a.pdf", 2), "a (2).pdf");
        assert_eq!(numbered("notes", 3), "notes (3)");
        assert_eq!(numbered(".profile", 2), ".profile (2)");
    }
}
