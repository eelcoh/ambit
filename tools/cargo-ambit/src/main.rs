//! `cargo ambit`: the ambit checks for any workspace that follows the ambit layout.
//!
//! - `cargo ambit check`: everything CI must run before merge.
//! - `cargo ambit report`: writes the attack-surface report (`attack-surface.md`), which `check`
//!   requires to be up to date.
//! - `cargo ambit init --owner @someone`: adds the human-owned files a project needs.
//!
//! Layout: agent-owned apps under `apps/<name>/`, each with a human-owned `allowed-deps.txt`;
//! everything else is human-owned (CODEOWNERS). The rules themselves (which std APIs an app may
//! not use) are built into this tool, so they're versioned with ambit, not with the project.

#![forbid(unsafe_code)]

mod apps;
mod report;

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::{env, fs};

const CLIPPY_TOML: &str = include_str!("../app-rules.toml");

/// Lint levels for app crates. On the command line, so an app's own source can't relax them:
/// an `#[allow]` against a command-line forbid is a hard error (E0453).
const APP_LINTS: &[&str] = &[
    "-D",
    "warnings",
    "-F",
    "unsafe_code",
    "-F",
    "clippy::disallowed_methods",
    "-F",
    "clippy::disallowed_types",
    "-F",
    "clippy::print_stdout",
    "-F",
    "clippy::print_stderr",
];

const USAGE: &str = "usage: cargo ambit check
       cargo ambit report
       cargo ambit init --owner @github-user";

fn main() -> ExitCode {
    // As a cargo subcommand, cargo passes "ambit" as the first argument.
    let args: Vec<String> = env::args().skip(1).skip_while(|a| a == "ambit").collect();
    let result = match args.iter().map(String::as_str).collect::<Vec<_>>()[..] {
        ["check"] => check(),
        ["report"] => report::write().map(|path| println!("wrote {}", path.display())),
        ["init", "--owner", owner] => init(owner),
        _ => Err(USAGE.to_string()),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("cargo ambit: {e}");
            ExitCode::FAILURE
        }
    }
}

fn check() -> Result<(), String> {
    let meta = apps::Metadata::load()?;
    let root = meta.workspace_root();

    run("cargo", &["fmt", "--all", "--check"], &[])?;
    run(
        "cargo",
        &[
            "clippy",
            "--workspace",
            "--all-targets",
            "--locked",
            "--",
            "-D",
            "warnings",
        ],
        &[],
    )?;

    // The app rules, from this tool, not from the project.
    let conf_dir = meta.target_directory().join("ambit-clippy");
    fs::create_dir_all(&conf_dir).map_err(|e| format!("{}: {e}", conf_dir.display()))?;
    fs::write(conf_dir.join("clippy.toml"), CLIPPY_TOML).map_err(|e| e.to_string())?;
    let conf = conf_dir.to_string_lossy().into_owned();
    for app in meta.apps() {
        let manifest = app.manifest_path.to_string_lossy().into_owned();
        let mut args = vec![
            "clippy",
            "--manifest-path",
            &manifest,
            "--lib",
            "--locked",
            "--",
        ];
        args.extend(APP_LINTS);
        run("cargo", &args, &[("CLIPPY_CONF_DIR", &conf)])?;
    }

    run("cargo", &["test", "--workspace", "--locked"], &[])?;

    let failures = apps::check(&meta)?;
    if failures > 0 {
        return Err(format!("app checks: {failures} problem(s)"));
    }

    if !root.join("cackle.toml").exists() {
        return Err("no cackle.toml: level 2 (per-crate API permissions) is required".into());
    }
    run("cargo", &["acl", "-n", "--fail-on-warnings"], &[])?;
    report::check()?;
    println!("cargo ambit check: OK");
    Ok(())
}

fn run(program: &str, args: &[&str], envs: &[(&str, &str)]) -> Result<(), String> {
    eprintln!("+ {program} {}", args.join(" "));
    let status = Command::new(program)
        .args(args)
        .envs(envs.iter().copied())
        .status()
        .map_err(|e| format!("{program}: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("failed: {program} {}", args.join(" ")))
    }
}

/// Files `init` writes, relative to the workspace root. Existing files are left alone.
const TEMPLATES: &[(&str, &str)] = &[
    (
        ".github/CODEOWNERS",
        include_str!("../templates/CODEOWNERS"),
    ),
    (
        ".github/workflows/ambit.yml",
        include_str!("../templates/ambit.yml"),
    ),
    ("cackle.toml", include_str!("../templates/cackle.toml")),
];

fn init(owner: &str) -> Result<(), String> {
    if !owner.starts_with('@') || owner.len() < 2 || owner.contains(char::is_whitespace) {
        return Err(format!(
            "--owner {owner}: expected a GitHub handle like @someone"
        ));
    }
    let root = workspace_root()?;
    for (path, template) in TEMPLATES {
        let path = root.join(path);
        if path.exists() {
            println!("kept     {}", rel(&root, &path));
            continue;
        }
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        fs::write(&path, template.replace("@OWNER", owner))
            .map_err(|e| format!("{}: {e}", path.display()))?;
        println!("created  {}", rel(&root, &path));
    }
    let apps = root.join("apps");
    if !apps.exists() {
        println!("note     no apps/ yet: put each agent-owned app in apps/<name>/");
    }
    println!(
        "next     for each app, run `cargo ambit check` and copy the crates it reports into \
         apps/<name>/allowed-deps.txt after reviewing them; then `cargo acl` to set permissions"
    );
    Ok(())
}

fn workspace_root() -> Result<PathBuf, String> {
    Ok(apps::Metadata::load_no_deps()?
        .workspace_root()
        .to_path_buf())
}

fn rel(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .display()
        .to_string()
}
