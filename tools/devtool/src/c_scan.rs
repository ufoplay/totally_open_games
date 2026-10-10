//! A lightweight C scanner. It lists function definitions, function prototypes, `#define`s and top-level variable
//! initializers, with the identifiers each one references. It does not preprocess: both branches of an `#if` are
//! scanned, and macros are recorded instead of expanded.

use std::collections::BTreeSet;

/// A function definition.
#[derive(Debug, Clone)]
pub struct Function {
    pub name: String,
    pub is_static: bool,
    /// Identifiers referenced in the body.
    pub references: BTreeSet<String>,
    /// The definition's tokens joined by spaces: comments and formatting do not count as changes.
    pub tokens: String,
}

/// A `#define`, or a top-level variable with an initializer (a callback table, for instance).
#[derive(Debug, Clone)]
pub struct Symbol {
    pub name: String,
    pub is_static: bool,
    pub references: BTreeSet<String>,
}

/// What one C file defines and declares.
#[derive(Debug, Default, Clone)]
pub struct CFile {
    pub functions: Vec<Function>,
    /// Names of functions declared by a prototype.
    pub prototypes: BTreeSet<String>,
    pub macros: Vec<Symbol>,
    pub variables: Vec<Symbol>,
    /// Every identifier in the file, `#include` lines and macro parameters excepted.
    pub identifiers: BTreeSet<String>,
    /// A `{` is never closed: conditional compilation defeated the scanner, and later definitions may be missing.
    pub unbalanced: bool,
}

/// Whether `path` names a C or C++ source or header.
pub fn is_c_path(path: &str) -> bool {
    let extension = path.rsplit_once('.').map_or("", |(_, extension)| extension);
    ["c", "h", "cc", "cpp", "cxx", "hpp", "hh", "inl"].contains(&extension)
}

/// Words that may precede `(` without naming a function.
const KEYWORDS: &[&str] = &[
    "if",
    "for",
    "while",
    "switch",
    "return",
    "sizeof",
    "alignof",
    "_Alignof",
    "offsetof",
    "typeof",
    "decltype",
    "void",
    "char",
    "short",
    "int",
    "long",
    "float",
    "double",
    "signed",
    "unsigned",
    "bool",
    "_Bool",
    "const",
    "volatile",
    "static",
    "extern",
    "inline",
    "struct",
    "union",
    "enum",
    "typedef",
    "defined",
    "_Static_assert",
    "static_assert",
    "operator",
    "new",
    "delete",
    "case",
    "do",
    "else",
    "goto",
    "auto",
    "register",
    "restrict",
];

pub fn scan(source: &str) -> CFile {
    let (code, macros, directive_identifiers) = strip(source);
    let tokens = tokenize(&code);
    let mut file = CFile {
        macros,
        ..CFile::default()
    };
    file.identifiers = tokens
        .iter()
        .filter(|token| token.is_identifier())
        .map(|token| token.text.to_owned())
        .chain(directive_identifiers)
        .chain(
            file.macros
                .iter()
                .flat_map(|m| m.references.iter().cloned()),
        )
        .collect();
    Parser {
        tokens: &tokens,
        file: &mut file,
    }
    .run();
    file
}

#[derive(Debug, Clone, Copy)]
struct Token<'a> {
    text: &'a str,
}

impl Token<'_> {
    fn is_identifier(&self) -> bool {
        self.text
            .as_bytes()
            .first()
            .is_some_and(|byte| byte.is_ascii_alphabetic() || *byte == b'_')
    }

    fn is(&self, text: &str) -> bool {
        self.text == text
    }
}

/// Blanks comments, string and character literals and preprocessor lines, keeping line breaks. Returns the code, the
/// `#define`s and the identifiers used by the other directives (`#if` conditions, for instance), `#include`s aside.
fn strip(source: &str) -> (String, Vec<Symbol>, Vec<String>) {
    let bytes = source.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        let next = bytes.get(index + 1).copied();
        if byte == b'/' && next == Some(b'/') {
            while index < bytes.len() && bytes[index] != b'\n' {
                // A backslash-newline continues a line comment.
                if bytes[index] == b'\\' && bytes.get(index + 1) == Some(&b'\n') {
                    out.extend_from_slice(b" \\\n");
                    index += 2;
                    continue;
                }
                out.push(b' ');
                index += 1;
            }
        } else if byte == b'/' && next == Some(b'*') {
            out.extend_from_slice(b"  ");
            index += 2;
            while index < bytes.len()
                && !(bytes[index] == b'*' && bytes.get(index + 1) == Some(&b'/'))
            {
                out.push(if bytes[index] == b'\n' { b'\n' } else { b' ' });
                index += 1;
            }
            out.extend_from_slice(b"  ");
            index += 2;
        } else if byte == b'"' || (byte == b'\'' && !follows_digit(&out)) {
            out.push(b' ');
            index += 1;
            while index < bytes.len() && bytes[index] != byte && bytes[index] != b'\n' {
                if bytes[index] == b'\\' && index + 1 < bytes.len() {
                    out.push(if bytes[index + 1] == b'\n' {
                        b'\n'
                    } else {
                        b' '
                    });
                    out.push(b' ');
                    index += 2;
                    continue;
                }
                out.push(b' ');
                index += 1;
            }
            out.push(b' ');
            index += 1;
        } else {
            out.push(byte);
            index += 1;
        }
    }
    out.truncate(bytes.len());
    let stripped = String::from_utf8_lossy(&out).into_owned();

    // Logical lines: a directive with its backslash continuations, or one line of code.
    let mut logical: Vec<(String, usize, bool)> = Vec::new();
    let mut lines = stripped.split('\n');
    while let Some(line) = lines.next() {
        if !line.trim_start().starts_with('#') {
            logical.push((line.to_owned(), 1, false));
            continue;
        }
        let mut directive = line.to_owned();
        let mut count = 1;
        while directive.trim_end().ends_with('\\') {
            let Some(next) = lines.next() else { break };
            directive = directive.trim_end().trim_end_matches('\\').to_owned() + " " + next;
            count += 1;
        }
        let body = directive.trim_start()[1..].trim_start().to_owned();
        logical.push((body, count, true));
    }
    let kept = kept_branches(&logical);

    let mut code = String::with_capacity(stripped.len());
    let mut macros = Vec::new();
    let mut directive_identifiers = Vec::new();
    for ((text, count, is_directive), kept) in logical.iter().zip(kept) {
        if !is_directive {
            if kept {
                code.push_str(text);
            }
            code.push('\n');
            continue;
        }
        code.push_str(&"\n".repeat(*count));
        if let Some(definition) = text.strip_prefix("define") {
            macros.extend(parse_define(definition));
        } else if !text.starts_with("include") {
            directive_identifiers.extend(
                tokenize(text)
                    .iter()
                    .filter(|t| t.is_identifier())
                    .map(|t| t.text.to_owned()),
            );
        }
    }
    (code, macros, directive_identifiers)
}

/// Which logical lines of code survive conditional compilation, decided without evaluating conditions. In each
/// `#if` group, every branch is kept when each one balances its braces (so variant-only definitions stay visible);
/// otherwise only the first branch is, as when branches choose between two function headers. `#if 0` branches are
/// dropped.
fn kept_branches(lines: &[(String, usize, bool)]) -> Vec<bool> {
    struct Branch {
        start: usize,
        end: usize,
        braces: i32,
        disabled: bool,
    }
    let mut kept = vec![true; lines.len()];
    // The enclosing groups; the bottom one stands for the whole file.
    let mut groups: Vec<Vec<Branch>> = vec![vec![Branch {
        start: 0,
        end: lines.len(),
        braces: 0,
        disabled: false,
    }]];
    for (index, (text, _, is_directive)) in lines.iter().enumerate() {
        if !is_directive {
            let braces = text.matches('{').count() as i32 - text.matches('}').count() as i32;
            groups.last_mut().unwrap().last_mut().unwrap().braces += braces;
            continue;
        }
        let keyword: String = text.chars().take_while(char::is_ascii_alphabetic).collect();
        let condition = text[keyword.len()..].trim();
        match keyword.as_str() {
            "if" | "ifdef" | "ifndef" => groups.push(vec![Branch {
                start: index + 1,
                end: lines.len(),
                braces: 0,
                disabled: keyword == "if" && condition == "0",
            }]),
            "elif" | "elifdef" | "elifndef" | "else" if groups.len() > 1 => {
                let group = groups.last_mut().unwrap();
                group.last_mut().unwrap().end = index;
                group.push(Branch {
                    start: index + 1,
                    end: lines.len(),
                    braces: 0,
                    disabled: keyword == "elif" && condition == "0",
                });
            }
            "endif" if groups.len() > 1 => {
                let mut group = groups.pop().unwrap();
                group.last_mut().unwrap().end = index;
                let enabled: Vec<usize> =
                    (0..group.len()).filter(|&b| !group[b].disabled).collect();
                let balanced = enabled.iter().all(|&b| group[b].braces == 0);
                let mut braces = 0;
                for (position, branch) in group.iter().enumerate() {
                    let keep = !branch.disabled && (balanced || enabled.first() == Some(&position));
                    if keep {
                        braces += branch.braces;
                    } else {
                        kept[branch.start..branch.end].fill(false);
                    }
                }
                groups.last_mut().unwrap().last_mut().unwrap().braces += braces;
            }
            _ => {}
        }
    }
    kept
}

/// Whether the output so far ends in a digit, so that `'` is a C++14 digit separator.
fn follows_digit(out: &[u8]) -> bool {
    out.last().is_some_and(u8::is_ascii_alphanumeric)
        && out
            .iter()
            .rev()
            .take_while(|byte| byte.is_ascii_alphanumeric() || **byte == b'_')
            .last()
            .is_some_and(u8::is_ascii_digit)
}

/// Parses the text after `#define`.
fn parse_define(definition: &str) -> Option<Symbol> {
    let tokens = tokenize(definition);
    let name = tokens.first().filter(|token| token.is_identifier())?;
    let mut parameters = BTreeSet::new();
    let mut body = &tokens[1..];
    // A function-like macro has `(` right after its name.
    let name_end = definition
        .find(name.text)
        .map(|start| start + name.text.len());
    if name_end.is_some_and(|end| definition[end..].starts_with('(')) {
        let close = body.iter().position(|token| token.is(")"))?;
        parameters.extend(
            body[..close]
                .iter()
                .filter(|t| t.is_identifier())
                .map(|t| t.text),
        );
        body = &body[close + 1..];
    }
    Some(Symbol {
        name: name.text.to_owned(),
        is_static: false,
        references: body
            .iter()
            .filter(|token| token.is_identifier() && !parameters.contains(token.text))
            .map(|token| token.text.to_owned())
            .collect(),
    })
}

fn tokenize(code: &str) -> Vec<Token<'_>> {
    let bytes = code.as_bytes();
    let mut tokens = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        let start = index;
        if byte.is_ascii_whitespace() {
            index += 1;
            continue;
        }
        if byte.is_ascii_alphanumeric() || byte == b'_' {
            // Identifiers and numbers (with their suffixes and exponents) are one token each.
            while index < bytes.len()
                && (bytes[index].is_ascii_alphanumeric() || bytes[index] == b'_')
            {
                index += 1;
            }
        } else if byte.is_ascii() {
            index += 1;
        } else {
            index += code[index..].chars().next().map_or(1, char::len_utf8);
            continue;
        }
        tokens.push(Token {
            text: &code[start..index],
        });
    }
    tokens
}

struct Parser<'t, 'a> {
    tokens: &'t [Token<'a>],
    file: &'t mut CFile,
}

impl Parser<'_, '_> {
    /// Walks the top level. `extern "C" { … }` and `namespace … { … }` blocks are transparent.
    fn run(&mut self) {
        let tokens = self.tokens;
        let mut statement = 0;
        let mut index = 0;
        while index < tokens.len() {
            let token = tokens[index];
            if token.is(";") {
                self.declaration(&tokens[statement..index]);
                index += 1;
                statement = index;
            } else if token.is("}") {
                // The end of a transparent block.
                index += 1;
                statement = index;
            } else if token.is("{") {
                let head = &tokens[statement..index];
                let transparent = head.first().is_some_and(|first| first.is("namespace"))
                    || (head.len() == 1 && head[0].is("extern"));
                if transparent {
                    index += 1;
                    statement = index;
                    continue;
                }
                let end = matching(tokens, index).unwrap_or_else(|| {
                    self.file.unbalanced = true;
                    tokens.len()
                });
                if let Some(name) = function_name(head) {
                    self.file.functions.push(Function {
                        name: name.text.to_owned(),
                        is_static: is_static(head),
                        references: identifiers(&tokens[index..end]),
                        tokens: tokens[statement..end]
                            .iter()
                            .map(|token| token.text)
                            .collect::<Vec<_>>()
                            .join(" "),
                    });
                    index = end;
                    statement = index;
                } else {
                    // A struct, union or enum body, or an initializer: the statement continues to its `;`.
                    index = end;
                }
            } else {
                index += 1;
            }
        }
    }

    /// A top-level statement ending in `;`: a prototype, or a variable whose initializer is recorded.
    fn declaration(&mut self, statement: &[Token<'_>]) {
        let Some(first) = statement.first() else {
            return;
        };
        if first.is("typedef") {
            return;
        }
        if let Some(equals) = depth_zero_position(statement, "=") {
            let name = statement[..equals]
                .iter()
                .rev()
                .find(|token| token.is_identifier() && !KEYWORDS.contains(&token.text));
            if let Some(name) = name {
                self.file.variables.push(Symbol {
                    name: name.text.to_owned(),
                    is_static: is_static(statement),
                    references: identifiers(&statement[equals + 1..]),
                });
            }
            return;
        }
        if let Some(name) = prototype_name(statement) {
            self.file.prototypes.insert(name.to_owned());
        }
    }
}

/// The index just past the `}` matching the `{` at `open`.
fn matching(tokens: &[Token<'_>], open: usize) -> Option<usize> {
    let mut depth = 0;
    for (index, token) in tokens.iter().enumerate().skip(open) {
        if token.is("{") {
            depth += 1;
        } else if token.is("}") {
            depth -= 1;
            if depth == 0 {
                return Some(index + 1);
            }
        }
    }
    None
}

/// The position of `text` outside any bracket.
fn depth_zero_position(tokens: &[Token<'_>], text: &str) -> Option<usize> {
    let mut depth = 0i32;
    for (index, token) in tokens.iter().enumerate() {
        match token.text {
            "(" | "[" | "{" => depth += 1,
            ")" | "]" | "}" => depth -= 1,
            _ if depth == 0 && token.is(text) => return Some(index),
            _ => {}
        }
    }
    None
}

/// The name of a function definition whose head (the tokens before `{`) is `head`: `… name ( … ) qualifiers`.
fn function_name<'a>(head: &[Token<'a>]) -> Option<Token<'a>> {
    if depth_zero_position(head, "=").is_some() {
        return None;
    }
    let first = head.first()?;
    if ["struct", "union", "enum", "class", "typedef"].contains(&first.text)
        && !head.iter().any(|t| t.is("("))
    {
        return None;
    }
    // Skip trailing qualifiers such as `const` or `noexcept` back to the parameter list.
    let close = head.iter().rposition(|token| token.is(")"))?;
    if !head[close + 1..].iter().all(Token::is_identifier) {
        return None;
    }
    let open = opening_paren(head, close)?;
    let name = *head.get(open.checked_sub(1)?)?;
    (name.is_identifier() && !KEYWORDS.contains(&name.text)).then_some(name)
}

/// The name declared by a prototype statement, if it is one.
fn prototype_name<'a>(statement: &[Token<'a>]) -> Option<&'a str> {
    let mut depth = 0i32;
    for (index, token) in statement.iter().enumerate() {
        match token.text {
            "(" => {
                if depth == 0 && index > 0 {
                    let name = statement[index - 1];
                    let callable = name.is_identifier()
                        && !KEYWORDS.contains(&name.text)
                        && !name.text.starts_with("__")
                        && statement.get(index + 1).is_none_or(|next| !next.is("*"));
                    if callable {
                        return Some(name.text);
                    }
                }
                depth += 1;
            }
            ")" => depth -= 1,
            _ => {}
        }
    }
    None
}

/// The index of the `(` matching the `)` at `close`.
fn opening_paren(tokens: &[Token<'_>], close: usize) -> Option<usize> {
    let mut depth = 0;
    for index in (0..=close).rev() {
        if tokens[index].is(")") {
            depth += 1;
        } else if tokens[index].is("(") {
            depth -= 1;
            if depth == 0 {
                return Some(index);
            }
        }
    }
    None
}

fn is_static(head: &[Token<'_>]) -> bool {
    head.iter().any(|token| token.is("static"))
}

fn identifiers(tokens: &[Token<'_>]) -> BTreeSet<String> {
    tokens
        .iter()
        .filter(|token| token.is_identifier())
        .map(|token| token.text.to_owned())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SOURCE: &str = r#"
#include "landru/actor.h"
#define LANDRU_MAX 4 /* comment */
#define landru_draw(actor) xactor_Draw(actor, LANDRU_MAX)
#if defined(LANDRU_GAME_TIE)
#endif
#ifdef __cplusplus
extern "C" {
#endif
typedef struct Actor { int x; void (*draw)(int); } Actor;
typedef void (*Callback)(int);
int landru_count(void);
static const char* name = "a { b";
static int helper(int value) { return value + 1; } // helper(
static const Callback table[] = { kind_one, kind_two };
int landru_count(void)
{
    char c = '}';
    return helper(sizeof(Actor)) + table[0] != 0 && 1'000;
}
void (*landru_hook)(int);
#ifdef __cplusplus
}
#endif
"#;

    #[test]
    fn scans_definitions_prototypes_and_macros() {
        let file = scan(SOURCE);
        let names: Vec<_> = file
            .functions
            .iter()
            .map(|f| (f.name.as_str(), f.is_static))
            .collect();
        assert_eq!(names, [("helper", true), ("landru_count", false)]);
        assert!(file.functions[1].references.contains("helper"));
        assert!(file.functions[1].references.contains("table"));
        assert_eq!(file.prototypes.iter().collect::<Vec<_>>(), ["landru_count"]);
        let macros: Vec<_> = file.macros.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(macros, ["LANDRU_MAX", "landru_draw"]);
        let draw = &file.macros[1].references;
        assert_eq!(
            draw.iter().collect::<Vec<_>>(),
            ["LANDRU_MAX", "xactor_Draw"]
        );
        let table = file.variables.iter().find(|v| v.name == "table").unwrap();
        assert!(table.is_static);
        assert_eq!(
            table.references.iter().collect::<Vec<_>>(),
            ["kind_one", "kind_two"]
        );
        assert!(file.identifiers.contains("LANDRU_GAME_TIE"));
        assert!(file.identifiers.contains("xactor_Draw"));
        assert!(!file.identifiers.contains("actor"));
    }

    #[test]
    fn keeps_balanced_branches_and_the_first_unbalanced_one() {
        let file = scan(
            "#ifdef XW\nvoid draw(Actor* list) {\n#else\nvoid draw(void) {\n#endif\n    paint();\n}\n\
             #ifdef XW\nvoid only_xw(void) {}\n#else\nvoid only_tie(void) {}\n#endif\n\
             #if 0\nvoid dead(void) {}\n#endif\nvoid last(void) {}\n",
        );
        let names: Vec<_> = file.functions.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, ["draw", "only_xw", "only_tie", "last"]);
        assert!(!file.unbalanced);
        assert!(scan("void f(void) {\n").unbalanced);
        assert!(file.functions[0].references.contains("paint"));
    }

    #[test]
    fn formatting_and_comments_do_not_change_tokens() {
        let a = scan("int f(int x) { return x; }");
        let b = scan("int\nf(int x)\n{\n    /* same */ return x;\n}\n");
        assert_eq!(a.functions[0].tokens, b.functions[0].tokens);
    }
}
