//! The usage inventory (PLAN §1.2, §3): which functions of a building block the games use, which ones those reach,
//! and which ones are ported.
//!
//! A game uses a function when its sources name it, directly or through one of the block's macros or tables. A
//! function is reached when a used function's body names it, again through macros and tables (the callbacks a block
//! installs into itself). Matching is by identifier, so the result errs on the side of porting too much.

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::fmt::Write as _;

use anyhow::Result;

use crate::c_scan::{self, CFile};
use crate::git::Repo;
use crate::rust_scan::Ported;

/// Directories of a building block that are not part of it.
const BLOCK_EXCLUDED: &[&str] = &[
    "third_party/",
    "tools/",
    "examples/",
    "tests/",
    "test/",
    "cmake/",
    "docs/",
];
/// Where a building block keeps its public headers.
const PUBLIC_DIR: &str = "include/";
/// Where a game keeps its own sources; its building blocks are submodules and not listed.
const GAME_DIR: &str = "src/";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Status {
    Used,
    Reached,
    Unused,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Used => "used",
            Status::Reached => "reached",
            Status::Unused => "unused",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Entry {
    pub file: String,
    pub name: String,
    pub public: bool,
    pub status: Status,
    pub games: BTreeSet<String>,
    pub ported: bool,
}

/// A source tree at one commit.
pub struct Source<'a> {
    pub name: &'a str,
    pub repo: &'a mut Repo,
    pub commit: &'a str,
}

/// The building block's C files at its commit.
pub fn block_files(source: &mut Source<'_>) -> Result<Vec<(String, CFile)>> {
    let paths = source.repo.files(source.commit)?;
    let mut files = Vec::new();
    for path in paths {
        if !c_scan::is_c_path(&path)
            || BLOCK_EXCLUDED
                .iter()
                .any(|dir| path.starts_with(dir) || path.contains(&format!("/{dir}")))
        {
            continue;
        }
        if let Some(text) = source.repo.read(source.commit, &path)? {
            let file = c_scan::scan(&text);
            if file.unbalanced {
                eprintln!(
                    "warning: {}:{path}: unbalanced braces; the inventory may miss functions",
                    source.name
                );
            }
            files.push((path, file));
        }
    }
    Ok(files)
}

/// Every identifier in a game's own sources.
pub fn game_identifiers(source: &mut Source<'_>) -> Result<BTreeSet<String>> {
    let mut identifiers = BTreeSet::new();
    for path in source.repo.files(source.commit)? {
        if path.starts_with(GAME_DIR)
            && c_scan::is_c_path(&path)
            && let Some(text) = source.repo.read(source.commit, &path)?
        {
            identifiers.extend(c_scan::scan(&text).identifiers);
        }
    }
    Ok(identifiers)
}

/// Computes the inventory of `block` against `games`.
pub fn inventory(
    block: &mut Source<'_>,
    games: &mut [Source<'_>],
    ported: &Ported,
) -> Result<Vec<Entry>> {
    let files = block_files(block)?;
    let graph = Graph::new(&files);
    let mut games_of: HashMap<usize, BTreeSet<String>> = HashMap::new();
    for game in games.iter_mut() {
        let identifiers = game_identifiers(game)?;
        let mut used = BTreeSet::new();
        for identifier in &identifiers {
            graph.expand(None, identifier, &mut used);
        }
        for function in used {
            games_of
                .entry(function)
                .or_default()
                .insert(game.name.to_owned());
        }
    }
    // Everything reachable from the used functions.
    let mut reached: BTreeSet<usize> = games_of.keys().copied().collect();
    let mut queue: VecDeque<usize> = reached.iter().copied().collect();
    while let Some(function) = queue.pop_front() {
        let (file, index) = graph.functions[function];
        let mut next = BTreeSet::new();
        for identifier in &files[file].1.functions[index].references {
            graph.expand(Some(file), identifier, &mut next);
        }
        for function in next {
            if reached.insert(function) {
                queue.push_back(function);
            }
        }
    }
    let public: BTreeSet<&str> = files
        .iter()
        .filter(|(path, _)| path.starts_with(PUBLIC_DIR))
        .flat_map(|(_, file)| {
            file.prototypes
                .iter()
                .map(String::as_str)
                .chain(file.functions.iter().map(|f| f.name.as_str()))
        })
        .collect();
    let mut entries: Vec<Entry> = Vec::new();
    let mut positions: HashMap<(usize, &str), usize> = HashMap::new();
    for (id, &(file, index)) in graph.functions.iter().enumerate() {
        let (path, c_file) = &files[file];
        let function = &c_file.functions[index];
        let games = games_of.remove(&id).unwrap_or_default();
        let status = if !games.is_empty() {
            Status::Used
        } else if reached.contains(&id) {
            Status::Reached
        } else {
            Status::Unused
        };
        let entry = Entry {
            file: path.clone(),
            name: function.name.clone(),
            public: !function.is_static && public.contains(function.name.as_str()),
            status,
            games,
            ported: ported.contains(path, &function.name, function.is_static),
        };
        // A function defined under several `#if` branches of one file is one entry.
        match positions.get(&(file, function.name.as_str())) {
            Some(&position) => {
                let merged = &mut entries[position];
                merged.status = merged.status.min(entry.status);
                merged.games.extend(entry.games);
                merged.public |= entry.public;
            }
            None => {
                positions.insert((file, &function.name), entries.len());
                entries.push(entry);
            }
        }
    }
    Ok(entries)
}

/// Name resolution over the block: functions, macros and variables with initializers.
struct Graph<'f> {
    files: &'f [(String, CFile)],
    /// `(file, index in the file's functions)` for each function id.
    functions: Vec<(usize, usize)>,
    global_functions: HashMap<&'f str, Vec<usize>>,
    static_functions: HashMap<(usize, &'f str), usize>,
    /// Macros and global variables, by name: `(file, references)`.
    global_symbols: HashMap<&'f str, Vec<(usize, &'f BTreeSet<String>)>>,
    static_variables: HashMap<(usize, &'f str), &'f BTreeSet<String>>,
}

impl<'f> Graph<'f> {
    fn new(files: &'f [(String, CFile)]) -> Self {
        let mut graph = Graph {
            files,
            functions: Vec::new(),
            global_functions: HashMap::new(),
            static_functions: HashMap::new(),
            global_symbols: HashMap::new(),
            static_variables: HashMap::new(),
        };
        for (file, (_, c_file)) in files.iter().enumerate() {
            for (index, function) in c_file.functions.iter().enumerate() {
                let id = graph.functions.len();
                graph.functions.push((file, index));
                if function.is_static {
                    graph.static_functions.insert((file, &function.name), id);
                } else {
                    graph
                        .global_functions
                        .entry(&function.name)
                        .or_default()
                        .push(id);
                }
            }
            for symbol in &c_file.macros {
                graph
                    .global_symbols
                    .entry(&symbol.name)
                    .or_default()
                    .push((file, &symbol.references));
            }
            for symbol in &c_file.variables {
                if symbol.is_static {
                    graph
                        .static_variables
                        .insert((file, &symbol.name), &symbol.references);
                } else {
                    graph
                        .global_symbols
                        .entry(&symbol.name)
                        .or_default()
                        .push((file, &symbol.references));
                }
            }
        }
        graph
    }

    /// Adds the functions `identifier` names, as seen from `file` (`None` for a game), to `out`. Macros and
    /// variables are followed through; function bodies are not.
    fn expand(&self, file: Option<usize>, identifier: &str, out: &mut BTreeSet<usize>) {
        let mut seen = BTreeSet::new();
        let mut stack = vec![(file, identifier.to_owned())];
        while let Some((file, identifier)) = stack.pop() {
            if !seen.insert((file, identifier.clone())) {
                continue;
            }
            let local =
                file.and_then(|file| self.static_functions.get(&(file, identifier.as_str())));
            match local {
                Some(&function) => {
                    out.insert(function);
                }
                None => out.extend(
                    self.global_functions
                        .get(identifier.as_str())
                        .into_iter()
                        .flatten(),
                ),
            }
            let variable =
                file.and_then(|file| self.static_variables.get(&(file, identifier.as_str())));
            if let (Some(file), Some(references)) = (file, variable) {
                stack.extend(references.iter().map(|r| (Some(file), r.clone())));
            }
            for &(owner, references) in self
                .global_symbols
                .get(identifier.as_str())
                .into_iter()
                .flatten()
            {
                // Macro bodies resolve in the file that expands them, falling back to globals.
                let context = if self.files[owner].0.ends_with(".h") {
                    file
                } else {
                    Some(owner)
                };
                stack.extend(references.iter().map(|r| (context, r.clone())));
            }
        }
    }
}

/// Renders `USAGE.toml`.
pub fn render(block: &str, commits: &[(String, String)], entries: &[Entry]) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "# Usage inventory of {block}, generated by `just usage`. Do not edit."
    );
    out.push_str(
        "#\n\
         # One table per C file, one line per function, in source order:\n\
         # - status: \"used\" (named by a game), \"reached\" (named by a used or reached function, also through macros\n\
         #   and tables) or \"unused\" (PLAN §1.2);\n\
         # - games: the games that use it;\n\
         # - public: declared in a public header;\n\
         # - ported: a Rust item carries its name as `#[doc(alias)]`, or is named like it in the module porting its file.\n\n",
    );
    out.push_str("[commits]\n");
    for (repo, commit) in commits {
        let _ = writeln!(out, "{repo} = \"{commit}\"");
    }
    out.push_str("\n[summary]\n");
    for status in [Status::Used, Status::Reached, Status::Unused] {
        let total = entries.iter().filter(|e| e.status == status).count();
        let ported = entries
            .iter()
            .filter(|e| e.status == status && e.ported)
            .count();
        let _ = writeln!(
            out,
            "{} = {{ functions = {total}, ported = {ported} }}",
            status.as_str()
        );
    }
    let mut by_file: BTreeMap<&str, Vec<&Entry>> = BTreeMap::new();
    for entry in entries {
        by_file.entry(&entry.file).or_default().push(entry);
    }
    for (file, entries) in by_file {
        let _ = writeln!(out, "\n[\"{file}\"]");
        for entry in entries {
            let mut fields = vec![format!("status = \"{}\"", entry.status.as_str())];
            if !entry.games.is_empty() {
                let games: Vec<String> = entry.games.iter().map(|g| format!("\"{g}\"")).collect();
                fields.push(format!("games = [{}]", games.join(", ")));
            }
            if entry.public {
                fields.push("public = true".to_owned());
            }
            fields.push(format!("ported = {}", entry.ported));
            let _ = writeln!(out, "{} = {{ {} }}", entry.name, fields.join(", "));
        }
    }
    out
}
