//! Workspace tooling behind the `just` recipes (PLAN §3, §5). Invoke it through `just`, not directly.

mod c_scan;
mod git;
mod rust_scan;
mod upstream;
mod usage;
mod workspace;

use anyhow::{Result, bail};

use workspace::Workspace;

const USAGE: &str = "\
usage: devtool <command> [arguments]

  upstream-fetch                 clone or update the upstream repositories in .upstream/
  upstream-status [crate]        commits since the pin: changed ported, changed unported and newly used functions
  upstream-diff <crate> [target] upstream diff since the pin, of one file or of one function's history
  upstream-bump <crate> <sha>    move the pin
  usage [crate]                  regenerate USAGE.toml
  harness-export                 export the building blocks at their pins into target/harness/upstream/";

fn main() -> std::process::ExitCode {
    match run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error:#}");
            std::process::ExitCode::FAILURE
        }
    }
}

fn run() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let workspace = Workspace::load()?;
    match args.as_slice() {
        ["upstream-fetch"] => upstream::fetch(&workspace),
        ["harness-export"] => upstream::export_for_harness(&workspace),
        ["upstream-status", rest @ ..] if rest.len() <= 1 => {
            for krate in workspace.blocks(rest.first().copied())? {
                upstream::status(&workspace, krate)?;
                println!();
            }
            Ok(())
        }
        ["upstream-diff", krate, rest @ ..] if rest.len() <= 1 => {
            upstream::diff(&workspace, workspace.find(krate)?, rest.first().copied())
        }
        ["upstream-bump", krate, revision] => {
            upstream::bump(&workspace, workspace.find(krate)?, revision)
        }
        ["usage", rest @ ..] if rest.len() <= 1 => {
            for krate in workspace.blocks(rest.first().copied())? {
                upstream::write_usage(&workspace, krate)?;
            }
            Ok(())
        }
        _ => bail!("{USAGE}"),
    }
}
