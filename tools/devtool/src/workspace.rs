//! The workspace crates and their `[package.metadata.upstream]` tables.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use toml_edit::DocumentMut;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// A building block in `crates/`.
    Block,
    /// A game in `bin/`.
    Game,
}

#[derive(Debug, Clone)]
pub struct Crate {
    pub name: String,
    pub kind: Kind,
    pub dir: PathBuf,
    pub repository: String,
    /// The repository name, which is also its directory in `.upstream/`.
    pub repo_name: String,
    /// The pinned commit, once the crate ports its first function.
    pub commit: Option<String>,
}

pub struct Workspace {
    pub root: PathBuf,
    pub crates: Vec<Crate>,
    /// The PLAN §0 snapshot, keyed by repository name.
    snapshot: BTreeMap<String, String>,
}

impl Workspace {
    pub fn load() -> Result<Self> {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()?;
        let manifest = read_manifest(&root.join("Cargo.toml"))?;
        let snapshot = manifest["workspace"]["metadata"]["upstream"]["snapshot"]
            .as_table_like()
            .context("Cargo.toml: no [workspace.metadata.upstream.snapshot]")?
            .iter()
            .filter_map(|(repo, commit)| Some((repo.to_owned(), commit.as_str()?.to_owned())))
            .collect();
        let mut crates = Vec::new();
        for (parent, kind) in [("crates", Kind::Block), ("bin", Kind::Game)] {
            let mut dirs: Vec<PathBuf> = std::fs::read_dir(root.join(parent))?
                .map(|entry| entry.map(|entry| entry.path()))
                .collect::<Result<_, _>>()?;
            dirs.sort();
            for dir in dirs {
                let path = dir.join("Cargo.toml");
                if !path.exists() {
                    continue;
                }
                let manifest = read_manifest(&path)?;
                let package = &manifest["package"];
                let Some(upstream) = package
                    .get("metadata")
                    .and_then(|metadata| metadata.get("upstream"))
                else {
                    continue;
                };
                let Some(repository) = upstream.get("repository").and_then(|r| r.as_str()) else {
                    continue;
                };
                crates.push(Crate {
                    name: package["name"].as_str().context("package name")?.to_owned(),
                    kind,
                    dir,
                    repository: repository.to_owned(),
                    repo_name: repository
                        .trim_end_matches('/')
                        .rsplit('/')
                        .next()
                        .unwrap()
                        .to_owned(),
                    commit: upstream
                        .get("commit")
                        .and_then(|commit| commit.as_str())
                        .map(str::to_owned),
                });
            }
        }
        Ok(Self {
            root,
            crates,
            snapshot,
        })
    }

    pub fn find(&self, name: &str) -> Result<&Crate> {
        match self
            .crates
            .iter()
            .find(|c| c.name == name || c.repo_name == name)
        {
            Some(found) => Ok(found),
            None => bail!(
                "unknown crate {name}; expected one of {}",
                self.crates
                    .iter()
                    .map(|c| c.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        }
    }

    /// The named building block, or every building block.
    pub fn blocks(&self, name: Option<&str>) -> Result<Vec<&Crate>> {
        match name {
            Some(name) => {
                let found = self.find(name)?;
                if found.kind != Kind::Block {
                    bail!("{name} is a game, not a building block");
                }
                Ok(vec![found])
            }
            None => Ok(self
                .crates
                .iter()
                .filter(|c| c.kind == Kind::Block)
                .collect()),
        }
    }

    pub fn games(&self) -> impl Iterator<Item = &Crate> {
        self.crates.iter().filter(|c| c.kind == Kind::Game)
    }

    /// The pinned commit, or the snapshot commit until the crate pins its own.
    pub fn base_commit(&self, krate: &Crate) -> Result<(String, &'static str)> {
        if let Some(commit) = &krate.commit {
            return Ok((commit.clone(), "pin"));
        }
        let commit = self
            .snapshot
            .get(&krate.repo_name)
            .with_context(|| format!("no snapshot commit for {}", krate.repo_name))?;
        Ok((commit.clone(), "snapshot"))
    }

    pub fn upstream_dir(&self, krate: &Crate) -> PathBuf {
        self.root.join(".upstream").join(&krate.repo_name)
    }
}

pub fn read_manifest(path: &Path) -> Result<DocumentMut> {
    let text = std::fs::read_to_string(path).with_context(|| path.display().to_string())?;
    text.parse().with_context(|| path.display().to_string())
}
