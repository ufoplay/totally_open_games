//! Finds which C functions a crate ports.
//!
//! A `#[doc(alias = "…")]` names a C function anywhere in the crate, and so does a Rust function named like it
//! (PLAN §9 #13) — but only within the modules whose `//! Port of` header lists that function's C file. A
//! non-static C function also counts as ported through an alias anywhere; a static one, whose name other files may
//! reuse, does not.

use std::collections::BTreeSet;
use std::path::Path;

use anyhow::{Context, Result};
use syn::visit::Visit;

/// The ported names of one crate.
#[derive(Debug, Default)]
pub struct Ported {
    pub aliases: BTreeSet<String>,
    /// For each Rust module: the C files its `//! Port of` header lists, and its function names and aliases.
    pub modules: Vec<(Vec<String>, BTreeSet<String>)>,
}

impl Ported {
    /// Whether the C function `name`, defined in `c_file`, is ported.
    pub fn contains(&self, c_file: &str, name: &str, is_static: bool) -> bool {
        (!is_static && self.aliases.contains(name))
            || self.modules.iter().any(|(files, names)| {
                names.contains(name) && files.iter().any(|pattern| path_matches(pattern, c_file))
            })
    }
}

pub fn scan_crate(source_dir: &Path) -> Result<Ported> {
    let mut ported = Ported::default();
    let mut files = Vec::new();
    rust_files(source_dir, &mut files)?;
    for path in files {
        let text = std::fs::read_to_string(&path).with_context(|| path.display().to_string())?;
        let syntax = syn::parse_file(&text).with_context(|| path.display().to_string())?;
        let mut visitor = Visitor::default();
        visitor.visit_file(&syntax);
        let mut names = visitor.functions;
        names.extend(visitor.aliases.iter().cloned());
        ported.aliases.extend(visitor.aliases);
        ported.modules.push((port_of(&text), names));
    }
    Ok(ported)
}

fn rust_files(dir: &Path, out: &mut Vec<std::path::PathBuf>) -> Result<()> {
    if !dir.exists() {
        return Ok(());
    }
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            rust_files(&path, out)?;
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            out.push(path);
        }
    }
    out.sort();
    Ok(())
}

/// The backquoted C paths of the `//! Port of …` line, e.g. `src/asset/lfd.c`.
fn port_of(text: &str) -> Vec<String> {
    // The paragraph that starts with "Port of", up to the first blank doc line.
    let paragraph: String = text
        .lines()
        .take_while(|line| line.starts_with("//!"))
        .map(|line| line.trim_start_matches("//!").trim())
        .skip_while(|line| !line.starts_with("Port of"))
        .take_while(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    paragraph
        .split('`')
        .skip(1)
        .step_by(2)
        .filter(|path| path.contains('.'))
        .map(str::to_owned)
        .collect()
}

/// Matches a C path against a `Port of` path, where `*` stands for any run of characters within a component.
fn path_matches(pattern: &str, path: &str) -> bool {
    match pattern.split_once('*') {
        None => pattern == path,
        Some((prefix, suffix)) => {
            path.len() >= prefix.len() + suffix.len()
                && path.starts_with(prefix)
                && path.ends_with(suffix)
                && !path[prefix.len()..path.len() - suffix.len()].contains('/')
        }
    }
}

#[derive(Default)]
struct Visitor {
    aliases: BTreeSet<String>,
    functions: BTreeSet<String>,
}

impl<'ast> Visit<'ast> for Visitor {
    fn visit_attribute(&mut self, attribute: &'ast syn::Attribute) {
        if attribute.path().is_ident("doc") {
            // `#[doc(alias = "a")]` and `#[doc(alias("a", "b"))]`.
            let _ = attribute.parse_nested_meta(|meta| {
                if meta.path.is_ident("alias") {
                    if meta.input.peek(syn::Token![=]) {
                        let value: syn::LitStr = meta.value()?.parse()?;
                        self.aliases.insert(value.value());
                    } else {
                        let content;
                        syn::parenthesized!(content in meta.input);
                        let list = content.parse_terminated(
                            <syn::LitStr as syn::parse::Parse>::parse,
                            syn::Token![,],
                        )?;
                        self.aliases.extend(list.iter().map(syn::LitStr::value));
                    }
                }
                Ok(())
            });
        }
    }

    fn visit_signature(&mut self, signature: &'ast syn::Signature) {
        self.functions.insert(signature.ident.to_string());
        syn::visit::visit_signature(self, signature);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_aliases_and_port_headers() {
        let text = r#"//! Port of `src/asset/lfd.c`, `include/aeron/asset/*.h`.
#[doc(alias = "AeronLfd_Parse")]
pub fn parse() {}
#[doc(alias("AeronLfd", "AeronLfd_Free"))]
pub struct Lfd;
impl Lfd {
    fn decode_error() {}
}
"#;
        let mut visitor = Visitor::default();
        visitor.visit_file(&syn::parse_file(text).unwrap());
        let mut names = visitor.functions;
        names.extend(visitor.aliases.iter().cloned());
        let ported = Ported {
            aliases: visitor.aliases,
            modules: vec![(port_of(text), names)],
        };
        assert!(ported.contains("src/other.c", "AeronLfd_Free", false));
        assert!(
            !ported.contains("src/other.c", "AeronLfd_Free", true),
            "a static name is file-scoped"
        );
        assert!(ported.contains("src/asset/lfd.c", "AeronLfd_Free", true));
        assert!(ported.contains("src/asset/lfd.c", "decode_error", true));
        assert!(ported.contains("include/aeron/asset/lfd.h", "decode_error", true));
        assert!(!ported.contains("src/other.c", "decode_error", true));
        assert!(!ported.contains("src/asset/lfd.c", "AeronLfd_Find", false));
    }
}
