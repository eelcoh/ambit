//! What each app crate (every workspace member directly under `apps/`) can pull in.
//!
//! - An app has no build script and is not a proc macro: both run at build time with full
//!   authority.
//! - Everything an app depends on (normal and build edges, transitively; not dev) is in its
//!   `allowed-deps.txt`, and nothing listed there is unused.
//! - Every dependency comes from crates.io, from ambit's own repository, or is a workspace
//!   member: an allowlisted name from another git repo or a stray path is a different crate.
//! - Every directory under `apps/` with a manifest is a workspace member, or it would escape
//!   these checks.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

const CRATES_IO: &str = "registry+https://github.com/rust-lang/crates.io-index";
/// ambit's crates, when a project depends on them by git.
const AMBIT_GIT: &str = "git+https://github.com/eelcoh/ambit";
/// The targets apps are built for. Dependencies behind other cfgs never end up in an artifact.
const TARGETS: &[&str] = &[
    "x86_64-unknown-linux-gnu",
    "aarch64-unknown-linux-gnu",
    "x86_64-apple-darwin",
    "aarch64-apple-darwin",
    "x86_64-unknown-hermit",
];

pub struct Metadata(Value);

pub struct App {
    pub id: String,
    pub name: String,
    pub manifest_path: PathBuf,
}

impl Metadata {
    pub fn load() -> Result<Self, String> {
        let mut args = vec!["metadata", "--format-version", "1", "--locked"];
        for target in TARGETS {
            args.extend(["--filter-platform", target]);
        }
        Self::run(&args)
    }

    pub fn load_no_deps() -> Result<Self, String> {
        Self::run(&["metadata", "--format-version", "1", "--no-deps"])
    }

    fn run(args: &[&str]) -> Result<Self, String> {
        let output = Command::new("cargo")
            .args(args)
            .output()
            .map_err(|e| format!("cargo metadata: {e}"))?;
        if !output.status.success() {
            return Err(format!(
                "cargo metadata failed:\n{}",
                String::from_utf8_lossy(&output.stderr)
            ));
        }
        serde_json::from_slice(&output.stdout)
            .map(Self)
            .map_err(|e| format!("cargo metadata: {e}"))
    }

    pub fn workspace_root(&self) -> &Path {
        Path::new(self.0["workspace_root"].as_str().unwrap_or("."))
    }

    pub fn target_directory(&self) -> PathBuf {
        PathBuf::from(self.0["target_directory"].as_str().unwrap_or("target"))
    }

    fn packages(&self) -> BTreeMap<&str, &Value> {
        self.0["packages"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|p| Some((p["id"].as_str()?, p)))
            .collect()
    }

    fn members(&self) -> BTreeSet<&str> {
        self.0["workspace_members"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect()
    }

    fn apps_dir(&self) -> PathBuf {
        self.workspace_root().join("apps")
    }

    /// Workspace members directly under `apps/`.
    pub fn apps(&self) -> Vec<App> {
        let packages = self.packages();
        let mut apps: Vec<App> = self
            .members()
            .into_iter()
            .filter_map(|id| {
                let p = packages.get(id)?;
                let manifest_path = PathBuf::from(p["manifest_path"].as_str()?);
                (manifest_path.parent()?.parent()? == self.apps_dir()).then(|| App {
                    id: id.to_string(),
                    name: p["name"].as_str().unwrap_or(id).to_string(),
                    manifest_path,
                })
            })
            .collect();
        apps.sort_by(|a, b| a.name.cmp(&b.name));
        apps
    }
}

/// Runs the checks, prints the result per app. Returns the number of problems.
pub fn check(meta: &Metadata) -> Result<usize, String> {
    let packages = meta.packages();
    let members = meta.members();
    let nodes: BTreeMap<&str, &Value> = meta.0["resolve"]["nodes"]
        .as_array()
        .ok_or("cargo metadata: no resolve graph")?
        .iter()
        .filter_map(|n| Some((n["id"].as_str()?, n)))
        .collect();
    let apps = meta.apps();
    let mut problems = 0;

    let member_dirs: BTreeSet<PathBuf> = apps
        .iter()
        .filter_map(|a| a.manifest_path.parent().map(Path::to_path_buf))
        .collect();
    if let Ok(entries) = std::fs::read_dir(meta.apps_dir()) {
        let mut dirs: Vec<PathBuf> = entries
            .filter_map(|e| Some(e.ok()?.path()))
            .filter(|d| d.join("Cargo.toml").exists() && !member_dirs.contains(d))
            .collect();
        dirs.sort();
        for dir in dirs {
            println!(
                "app checks failed: {} is not a workspace member",
                rel(meta, &dir)
            );
            problems += 1;
        }
    }

    for app in &apps {
        let mut errors = Vec::new();
        let package = packages[app.id.as_str()];
        for target in package["targets"].as_array().into_iter().flatten() {
            let kinds: Vec<&str> = target["kind"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .collect();
            if kinds
                .iter()
                .any(|k| *k == "custom-build" || *k == "proc-macro")
            {
                errors.push(format!(
                    "must not have a {} target ({})",
                    kinds.join("/"),
                    target["src_path"].as_str().unwrap_or("?")
                ));
            }
        }

        // Everything reachable from the app over normal and build edges.
        let mut seen = BTreeSet::new();
        let mut todo = vec![app.id.as_str()];
        while let Some(id) = todo.pop() {
            let deps = nodes.get(id).and_then(|n| n["deps"].as_array());
            for dep in deps.into_iter().flatten() {
                let normal_or_build = dep["dep_kinds"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .any(|k| k["kind"].is_null() || k["kind"].as_str() == Some("build"));
                if let Some(pkg) = dep["pkg"].as_str()
                    && normal_or_build
                    && seen.insert(pkg)
                {
                    todo.push(pkg);
                }
            }
        }

        for id in &seen {
            let p = packages[id];
            let source = p["source"].as_str().unwrap_or("");
            let trusted = source == CRATES_IO
                || source.starts_with(&format!("{AMBIT_GIT}?"))
                || source.starts_with(&format!("{AMBIT_GIT}.git?"));
            if !members.contains(id) && !trusted {
                let from = if source.is_empty() {
                    p["manifest_path"].as_str().unwrap_or("?")
                } else {
                    source
                };
                errors.push(format!(
                    "{} {} comes from {from}, not crates.io, ambit or the workspace",
                    p["name"].as_str().unwrap_or("?"),
                    p["version"].as_str().unwrap_or("?"),
                ));
            }
        }

        let actual: BTreeSet<&str> = seen
            .iter()
            .filter_map(|id| packages[id]["name"].as_str())
            .collect();
        let listing = app
            .manifest_path
            .parent()
            .unwrap_or(Path::new("."))
            .join("allowed-deps.txt");
        let text = std::fs::read_to_string(&listing).unwrap_or_default();
        let allowed: BTreeSet<&str> = text
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .collect();
        let listing = rel(meta, &listing);
        for dep in actual.difference(&allowed) {
            errors.push(format!("{dep} is not in {listing}"));
        }
        for dep in allowed.difference(&actual) {
            errors.push(format!(
                "{listing} lists {dep}, which {} no longer uses",
                app.name
            ));
        }

        if errors.is_empty() {
            println!(
                "app checks: {} OK ({} dependencies, all allowlisted, from trusted sources)",
                app.name,
                actual.len()
            );
        } else {
            println!("app checks failed for {}:", app.name);
            for e in &errors {
                println!("  {e}");
            }
            problems += errors.len();
        }
    }
    Ok(problems)
}

fn rel(meta: &Metadata, path: &Path) -> String {
    path.strip_prefix(meta.workspace_root())
        .unwrap_or(path)
        .display()
        .to_string()
}
