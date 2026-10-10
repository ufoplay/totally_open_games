//! The `git` command line (libgit2 would be a C library). Files are read straight from commits, without checkouts.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use anyhow::{Context, Result, bail};

/// One upstream clone.
pub struct Repo {
    pub dir: PathBuf,
    reader: Option<BlobReader>,
    cache: HashMap<String, Option<String>>,
}

impl Repo {
    pub fn open(dir: &Path) -> Result<Self> {
        if !dir.join(".git").exists() {
            bail!("{} is not cloned; run `just upstream-fetch`", dir.display());
        }
        Ok(Self {
            dir: dir.to_owned(),
            reader: None,
            cache: HashMap::new(),
        })
    }

    /// Runs git and returns its standard output.
    pub fn git(&self, args: &[&str]) -> Result<String> {
        git(Some(&self.dir), args)
    }

    /// The full hash of `revision`.
    pub fn resolve(&self, revision: &str) -> Result<String> {
        let commit = format!("{revision}^{{commit}}");
        Ok(self
            .git(&["rev-parse", "--verify", "--quiet", &commit])
            .with_context(|| format!("{}: unknown revision {revision}", self.dir.display()))?
            .trim()
            .to_owned())
    }

    /// The remote's default branch head, as fetched.
    pub fn remote_head(&self) -> Result<String> {
        ["origin/HEAD", "origin/main", "origin/master"]
            .iter()
            .find_map(|revision| self.resolve(revision).ok())
            .with_context(|| {
                format!(
                    "{}: no remote head; run `just upstream-fetch`",
                    self.dir.display()
                )
            })
    }

    /// The file paths of `commit`, submodules excluded.
    pub fn files(&self, commit: &str) -> Result<Vec<String>> {
        let listing = self.git(&["ls-tree", "-r", "-z", "--full-tree", commit])?;
        Ok(listing
            .split('\0')
            .filter_map(|entry| {
                let (meta, path) = entry.split_once('\t')?;
                meta.contains(" blob ").then(|| path.to_owned())
            })
            .collect())
    }

    /// The text of `path` at `commit`, or `None` when it does not exist there.
    pub fn read(&mut self, commit: &str, path: &str) -> Result<Option<String>> {
        let object = format!("{commit}:{path}");
        if let Some(text) = self.cache.get(&object) {
            return Ok(text.clone());
        }
        if self.reader.is_none() {
            self.reader = Some(BlobReader::spawn(&self.dir)?);
        }
        let text = self.reader.as_mut().unwrap().read(&object)?;
        self.cache.insert(object, text.clone());
        Ok(text)
    }
}

/// Runs git in `dir` and returns its standard output.
pub fn git(dir: Option<&Path>, args: &[&str]) -> Result<String> {
    let mut command = Command::new("git");
    if let Some(dir) = dir {
        command.arg("-C").arg(dir);
    }
    let output = command.args(args).output().context("running git")?;
    if !output.status.success() {
        bail!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Runs git in `dir` with its output going to the terminal.
pub fn git_inherit(dir: Option<&Path>, args: &[&str]) -> Result<()> {
    let mut command = Command::new("git");
    if let Some(dir) = dir {
        command.arg("-C").arg(dir);
    }
    let status = command.args(args).status().context("running git")?;
    if !status.success() {
        bail!("git {} failed", args.join(" "));
    }
    Ok(())
}

/// A long-running `git cat-file --batch`.
struct BlobReader {
    child: Child,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
}

impl BlobReader {
    fn spawn(dir: &Path) -> Result<Self> {
        let mut child = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["cat-file", "--batch"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .context("running git cat-file")?;
        let input = child.stdin.take().unwrap();
        let output = BufReader::new(child.stdout.take().unwrap());
        Ok(Self {
            child,
            input,
            output,
        })
    }

    fn read(&mut self, object: &str) -> Result<Option<String>> {
        writeln!(self.input, "{object}")?;
        self.input.flush()?;
        let mut header = String::new();
        self.output.read_line(&mut header)?;
        // `<hash> <type> <size>`, or `<object> missing`.
        let fields: Vec<&str> = header.split_whitespace().collect();
        if fields.last() == Some(&"missing") || fields.len() != 3 {
            return Ok(None);
        }
        let size: usize = fields[2].parse().context("cat-file header")?;
        let mut data = vec![0; size + 1];
        self.output.read_exact(&mut data)?;
        data.pop();
        Ok((fields[1] == "blob").then(|| String::from_utf8_lossy(&data).into_owned()))
    }
}

impl Drop for BlobReader {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
