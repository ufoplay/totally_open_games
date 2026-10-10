//! Upstream tracking (PLAN §3): fetch the clones, report what changed since a pin, show diffs and move pins.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result, bail};

use crate::c_scan;
use crate::git::{self, Repo};
use crate::rust_scan;
use crate::usage::{self, Entry, Source, Status};
use crate::workspace::{Crate, Workspace, read_manifest};

/// Clones the missing upstream repositories and fetches the others.
pub fn fetch(workspace: &Workspace) -> Result<()> {
    std::fs::create_dir_all(workspace.root.join(".upstream"))?;
    for krate in &workspace.crates {
        let dir = workspace.upstream_dir(krate);
        if dir.join(".git").exists() {
            println!("fetching {}", krate.repo_name);
            git::git_inherit(Some(&dir), &["fetch", "--quiet", "--prune", "origin"])?;
        } else {
            println!("cloning {}", krate.repo_name);
            let target = dir.to_str().context("non-UTF-8 path")?;
            git::git_inherit(None, &["clone", "--quiet", &krate.repository, target])?;
        }
    }
    Ok(())
}

/// The commits since a building block's pin, split into changed-ported, changed-unported and newly used functions.
pub fn status(workspace: &Workspace, krate: &Crate) -> Result<()> {
    let mut repo = Repo::open(&workspace.upstream_dir(krate))?;
    repo.git(&["fetch", "--quiet", "--prune", "origin"])?;
    let (base, origin) = workspace.base_commit(krate)?;
    let head = repo.remote_head()?;
    let ported = rust_scan::scan_crate(&krate.dir.join("src"))?;
    let commits: Vec<String> = repo
        .git(&[
            "rev-list",
            "--reverse",
            "--topo-order",
            &format!("{base}..{head}"),
        ])?
        .lines()
        .map(str::to_owned)
        .collect();
    println!(
        "{} ({}): {} commit(s) since {} ({origin}), head {}",
        krate.name,
        krate.repo_name,
        commits.len(),
        short(&base),
        short(&head)
    );

    // Functions touched by each commit, by comparing their tokens before and after.
    let mut changed: BTreeMap<(String, String), (bool, Vec<String>)> = BTreeMap::new();
    for commit in &commits {
        let parent = format!("{commit}^");
        let subject = repo
            .git(&["log", "-1", "--format=%h %s", commit])?
            .trim()
            .to_owned();
        let paths = repo.git(&["diff", "--name-only", &parent, commit])?;
        for path in paths.lines().filter(|path| c_scan::is_c_path(path)) {
            let before = functions_at(&mut repo, &parent, path)?;
            let after = functions_at(&mut repo, commit, path)?;
            let names: BTreeSet<&String> = before.keys().chain(after.keys()).collect();
            for name in names {
                let (old, new) = (before.get(name), after.get(name));
                if old.map(|f| &f.0) != new.map(|f| &f.0) {
                    let entry = changed.entry((path.to_owned(), name.clone())).or_default();
                    entry.0 = new.or(old).is_some_and(|f| f.1);
                    entry.1.push(subject.clone());
                }
            }
        }
    }
    let (changed_ported, changed_unported): (Vec<_>, Vec<_>) = changed
        .iter()
        .partition(|((path, name), (is_static, _))| ported.contains(path, name, *is_static));
    println!(
        "\nChanged ported functions (catch up): {}",
        changed_ported.len()
    );
    for ((path, name), (_, subjects)) in &changed_ported {
        println!("  {path} {name}");
        for subject in subjects {
            println!("      {subject}");
        }
    }
    println!(
        "\nChanged unported functions (ignored): {}",
        changed_unported.len()
    );

    // Functions the games use now that they did not use at their base commits.
    let before = inventory(workspace, krate, &mut repo, &base, &ported, false)?;
    let after = inventory(workspace, krate, &mut repo, &head, &ported, true)?;
    let was_used: BTreeSet<(&str, &str)> = before
        .iter()
        .filter(|entry| entry.status != Status::Unused)
        .map(|entry| (entry.file.as_str(), entry.name.as_str()))
        .collect();
    let newly_used: Vec<&Entry> = after
        .iter()
        .filter(|entry| entry.status != Status::Unused && !entry.ported)
        .filter(|entry| !was_used.contains(&(entry.file.as_str(), entry.name.as_str())))
        .collect();
    println!("\nNewly used functions (port): {}", newly_used.len());
    for entry in newly_used {
        let games: Vec<&str> = entry.games.iter().map(String::as_str).collect();
        let by = if games.is_empty() {
            "reached".to_owned()
        } else {
            format!("used by {}", games.join(", "))
        };
        println!("  {} {} ({by})", entry.file, entry.name);
    }
    Ok(())
}

/// The function tokens of `path` at `commit` and whether each function is static, by name. Definitions under
/// several `#if` branches are concatenated.
fn functions_at(
    repo: &mut Repo,
    commit: &str,
    path: &str,
) -> Result<BTreeMap<String, (String, bool)>> {
    let mut functions = BTreeMap::<String, (String, bool)>::new();
    if let Some(text) = repo.read(commit, path)? {
        for function in c_scan::scan(&text).functions {
            let entry = functions.entry(function.name).or_default();
            entry.0.push_str(&function.tokens);
            entry.1 = function.is_static;
        }
    }
    Ok(functions)
}

/// The inventory of `krate` at `commit`, against the games at their base commits or, with `games_at_head`, at their
/// remote heads.
fn inventory(
    workspace: &Workspace,
    krate: &Crate,
    repo: &mut Repo,
    commit: &str,
    ported: &rust_scan::Ported,
    games_at_head: bool,
) -> Result<Vec<Entry>> {
    let mut game_repos = Vec::new();
    for game in workspace.games() {
        let game_repo = Repo::open(&workspace.upstream_dir(game))?;
        let game_commit = if games_at_head {
            game_repo.remote_head()?
        } else {
            workspace.base_commit(game)?.0
        };
        game_repos.push((game.repo_name.clone(), game_repo, game_commit));
    }
    let mut games: Vec<Source<'_>> = game_repos
        .iter_mut()
        .map(|(name, repo, commit)| Source { name, repo, commit })
        .collect();
    let mut block = Source {
        name: &krate.repo_name,
        repo,
        commit,
    };
    usage::inventory(&mut block, &mut games, ported)
}

/// Regenerates `USAGE.toml` from the building block and the games at their base commits.
pub fn write_usage(workspace: &Workspace, krate: &Crate) -> Result<()> {
    let mut repo = Repo::open(&workspace.upstream_dir(krate))?;
    let (commit, _) = workspace.base_commit(krate)?;
    let ported = rust_scan::scan_crate(&krate.dir.join("src"))?;
    let entries = inventory(workspace, krate, &mut repo, &commit, &ported, false)?;
    let mut commits = vec![(krate.repo_name.clone(), commit)];
    for game in workspace.games() {
        commits.push((game.repo_name.clone(), workspace.base_commit(game)?.0));
    }
    let path = krate.dir.join("USAGE.toml");
    std::fs::write(&path, usage::render(&krate.repo_name, &commits, &entries))?;
    let count = |status| entries.iter().filter(|e| e.status == status).count();
    let ported_count = |status| {
        entries
            .iter()
            .filter(|e| e.status == status && e.ported)
            .count()
    };
    println!(
        "{}: {} used ({} ported), {} reached ({} ported), {} unused -> {}",
        krate.name,
        count(Status::Used),
        ported_count(Status::Used),
        count(Status::Reached),
        ported_count(Status::Reached),
        count(Status::Unused),
        path.strip_prefix(&workspace.root)
            .unwrap_or(&path)
            .display()
    );
    Ok(())
}

/// Shows the upstream diff since the pin: everything, one file, or the history of one function.
pub fn diff(workspace: &Workspace, krate: &Crate, target: Option<&str>) -> Result<()> {
    let mut repo = Repo::open(&workspace.upstream_dir(krate))?;
    let (base, _) = workspace.base_commit(krate)?;
    let head = repo.remote_head()?;
    let range = format!("{base}..{head}");
    let Some(target) = target else {
        return git::git_inherit(Some(&repo.dir), &["diff", &range]);
    };
    let files_at_head = repo.files(&head)?;
    let files_at_base = repo.files(&base)?;
    if files_at_head
        .iter()
        .chain(&files_at_base)
        .any(|path| path == target)
    {
        return git::git_inherit(Some(&repo.dir), &["diff", &range, "--", target]);
    }
    for (commit, files) in [(&head, &files_at_head), (&base, &files_at_base)] {
        for path in files.iter().filter(|path| c_scan::is_c_path(path)) {
            let Some(text) = repo.read(commit, path)? else {
                continue;
            };
            if c_scan::scan(&text)
                .functions
                .iter()
                .any(|function| function.name == target)
            {
                let line_range = format!(":{target}:{path}");
                return git::git_inherit(Some(&repo.dir), &["log", "-L", &line_range, &range]);
            }
        }
    }
    bail!(
        "{target} is neither a file nor a function of {}",
        krate.repo_name
    )
}

/// Moves the pin of `krate` to `revision`.
pub fn bump(workspace: &Workspace, krate: &Crate, revision: &str) -> Result<()> {
    let repo = Repo::open(&workspace.upstream_dir(krate))?;
    let commit = repo.resolve(revision)?;
    let path = krate.dir.join("Cargo.toml");
    let mut manifest = read_manifest(&path)?;
    let upstream = manifest["package"]["metadata"]["upstream"]
        .as_table_mut()
        .context("no [package.metadata.upstream] table")?;
    let previous = upstream
        .get("commit")
        .and_then(|item| item.as_str())
        .map(str::to_owned);
    upstream["commit"] = toml_edit::value(commit.as_str());
    std::fs::write(&path, manifest.to_string())?;
    println!(
        "{}: pin {} -> {}. Regenerate the inventory with `just usage {}`.",
        krate.name,
        previous.as_deref().map_or("(snapshot)", short),
        short(&commit),
        krate.name
    );
    Ok(())
}

fn short(commit: &str) -> &str {
    &commit[..commit.len().min(7)]
}

/// Exports every building block at its base commit into `target/harness/upstream/<Repo>` for the reference harness.
pub fn export_for_harness(workspace: &Workspace) -> Result<()> {
    for krate in workspace.blocks(None)? {
        let repo = Repo::open(&workspace.upstream_dir(krate))?;
        let (commit, origin) = workspace.base_commit(krate)?;
        let target = workspace
            .root
            .join("target/harness/upstream")
            .join(&krate.repo_name);
        if target.exists() {
            std::fs::remove_dir_all(&target)?;
        }
        std::fs::create_dir_all(&target)?;
        let mut archive = std::process::Command::new("git")
            .arg("-C")
            .arg(&repo.dir)
            .args(["archive", "--format=tar", &commit])
            .stdout(std::process::Stdio::piped())
            .spawn()
            .context("running git archive")?;
        let extracted = std::process::Command::new("tar")
            .arg("-x")
            .arg("-C")
            .arg(&target)
            .stdin(archive.stdout.take().unwrap())
            .status()
            .context("running tar")?;
        if !archive.wait()?.success() || !extracted.success() {
            bail!("exporting {} at {commit} failed", krate.repo_name);
        }
        std::fs::write(target.join(".commit"), format!("{commit}\n"))?;
        println!(
            "exported {} at {} ({origin})",
            krate.repo_name,
            short(&commit)
        );
    }
    Ok(())
}
