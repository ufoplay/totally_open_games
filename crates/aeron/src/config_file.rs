//! Port of `src/config_file.c`, `include/aeron/config_file.h`.
//!
//! YAML configuration documents converted to a typed node tree with source positions, path lookup
//! (`video.modes[0]`), copy-on-write mutation, overlays and a deterministic YAML writer.
//!
//! DEVIATION: YAML is parsed by `saphyr-parser` (YAML 1.2) instead of libyaml (YAML 1.1). Scalars are still typed
//! by Aeron's own rules below, so documents convert the same way; syntax error messages and some error positions
//! differ, input must be UTF-8 (libyaml also accepts UTF-16), and a redefined anchor is accepted where libyaml
//! reports a duplicate anchor.

use std::collections::HashMap;
use std::sync::Arc;

use saphyr_parser::{Event, Parser, ScalarStyle, ScanError, Span};

use crate::numeric::{format_ascii_double, is_c_space, parse_ascii_double};
use crate::vfs::{OpenMode, Vfs, VfsRoot};

#[doc(alias = "AERON_CONFIG_MAX_DEPTH")]
const CONFIG_MAX_DEPTH: usize = 128;

/// What went wrong in a configuration operation.
#[doc(alias = "AeronConfigErrorCode")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigErrorCode {
    #[doc(alias = "AERON_CONFIG_ERROR_NOT_FOUND")]
    NotFound,
    #[doc(alias = "AERON_CONFIG_ERROR_IO")]
    Io,
    #[doc(alias = "AERON_CONFIG_ERROR_YAML_SYNTAX")]
    YamlSyntax,
    #[doc(alias = "AERON_CONFIG_ERROR_MULTIPLE_DOCUMENTS")]
    MultipleDocuments,
    #[doc(alias = "AERON_CONFIG_ERROR_INVALID_ARGUMENT")]
    InvalidArgument,
    #[doc(alias = "AERON_CONFIG_ERROR_PATH_TYPE_CONFLICT")]
    PathTypeConflict,
    /// Also reported when a YAML document cannot be converted (a non-scalar map key or nesting deeper than 128
    /// levels), as upstream does.
    #[doc(alias = "AERON_CONFIG_ERROR_OUT_OF_MEMORY")]
    OutOfMemory,
}

/// A configuration error with its source location. `line` and `column` are one-based, or zero when unknown.
///
/// DEVIATION: `path` and `message` are not truncated to `AERON_CONFIG_ERROR_*_CAPACITY` (512) bytes.
#[doc(alias("AeronConfigError", "AeronConfig_ClearError"))]
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{path}:{line}:{column}: {message}")]
pub struct ConfigError {
    pub code: ConfigErrorCode,
    pub root: VfsRoot,
    pub path: String,
    pub line: u32,
    pub column: u32,
    pub message: String,
}

/// `AeronConfig_Fail`.
#[doc(alias = "AeronConfig_Fail")]
fn fail<T>(
    code: ConfigErrorCode,
    root: VfsRoot,
    path: &str,
    line: u32,
    column: u32,
    message: impl Into<String>,
) -> Result<T, ConfigError> {
    Err(ConfigError {
        code,
        root,
        path: path.to_owned(),
        line,
        column,
        message: message.into(),
    })
}

/// Runtime type of a node.
#[doc(alias = "AeronConfigNodeType")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigNodeType {
    #[doc(alias = "AERON_CONFIG_NULL")]
    Null,
    #[doc(alias = "AERON_CONFIG_BOOL")]
    Bool,
    #[doc(alias = "AERON_CONFIG_INT")]
    Int,
    #[doc(alias = "AERON_CONFIG_FLOAT")]
    Float,
    #[doc(alias = "AERON_CONFIG_STRING")]
    String,
    #[doc(alias = "AERON_CONFIG_MAP")]
    Map,
    #[doc(alias = "AERON_CONFIG_SEQUENCE")]
    Sequence,
}

/// A value to store with [`ConfigFile::set_value`]. Map entries keep their order.
#[doc(alias("AeronConfigValue", "AeronConfigMapValue"))]
#[derive(Debug, Clone, PartialEq)]
pub enum ConfigValue {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    String(String),
    Map(Vec<(String, ConfigValue)>),
    Sequence(Vec<ConfigValue>),
}

/// One key of a map node. Keys may repeat; lookups find the first.
#[doc(alias = "AeronConfigMapEntry")]
#[derive(Debug, Clone, PartialEq)]
pub struct ConfigMapEntry {
    pub key: String,
    pub value: ConfigNode,
}

/// The value of a node.
#[derive(Debug, Clone, PartialEq)]
pub enum ConfigNodeValue {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    String(String),
    Map(Vec<ConfigMapEntry>),
    Sequence(Vec<ConfigNode>),
}

/// A node of a configuration document with its source file and position. Its strings are owned, which covers
/// `AeronConfig_DuplicateString` and `AeronConfig_DuplicateBytes`.
#[doc(alias(
    "AeronConfigNode",
    "AeronConfigNode_Destroy",
    "AeronConfig_DuplicateString",
    "AeronConfig_DuplicateBytes"
))]
#[derive(Debug, Clone, PartialEq)]
pub struct ConfigNode {
    value: ConfigNodeValue,
    source_root: VfsRoot,
    source_path: Arc<str>,
    line: u32,
    column: u32,
}

/// A configuration document: a root node plus the VFS root and path it loads from and saves to.
#[doc(alias(
    "AeronConfigFile",
    "AeronConfigFile_Destroy",
    "AeronConfigFile_Clone",
    "AeronConfigNode_Clone"
))]
#[derive(Debug, Clone, PartialEq)]
pub struct ConfigFile {
    root: ConfigNode,
    root_kind: VfsRoot,
    path: String,
}

#[doc(alias("AeronConfig_EqualsIgnoreCase", "AeronConfig_LowerAscii"))]
fn equals_ignore_case(value: &str, expected: &str) -> bool {
    value.eq_ignore_ascii_case(expected)
}

/// `strtoll(text, &end, 0)` that must consume the whole text without overflow: optional leading whitespace and
/// sign, then `0x` hexadecimal, `0` octal or decimal digits.
#[doc(alias = "AeronConfig_ParseInt")]
fn parse_int(text: &str) -> Option<i64> {
    let bytes = text.as_bytes();
    let mut index = bytes.iter().take_while(|&&byte| is_c_space(byte)).count();
    let negative = match bytes.get(index) {
        Some(b'-') => {
            index += 1;
            true
        }
        Some(b'+') => {
            index += 1;
            false
        }
        _ => false,
    };
    let rest = &bytes[index..];
    let (radix, digits) = if rest.len() > 2
        && rest[0] == b'0'
        && (rest[1] == b'x' || rest[1] == b'X')
        && rest[2].is_ascii_hexdigit()
    {
        (16, &rest[2..])
    } else if rest.first() == Some(&b'0') {
        (8, rest)
    } else {
        (10, rest)
    };
    let count = digits
        .iter()
        .take_while(|&&byte| char::from(byte).is_digit(radix))
        .count();
    if count == 0 || count != digits.len() {
        return None;
    }
    let limit = 1u128 << 63;
    let mut magnitude = 0u128;
    for &byte in digits {
        magnitude = magnitude * u128::from(radix) + u128::from(char::from(byte).to_digit(radix)?);
        if magnitude > limit {
            return None;
        }
    }
    if negative {
        Some((magnitude as i128).wrapping_neg() as i64)
    } else {
        i64::try_from(magnitude).ok()
    }
}

#[doc(alias = "AeronConfig_ParseFloat")]
fn parse_float(text: &str) -> Option<f64> {
    parse_ascii_double(text)
}

#[doc(alias = "AeronConfig_TextLooksFloat")]
fn text_looks_float(text: &str) -> bool {
    text.contains(['.', 'e', 'E'])
}

/// A YAML node as libyaml's composer produces it, before Aeron types the scalars.
#[derive(Debug, Clone)]
enum YamlNode {
    Scalar {
        value: String,
        plain: bool,
        span: Span,
    },
    Sequence {
        items: Vec<YamlNode>,
        span: Span,
    },
    Mapping {
        pairs: Vec<(YamlNode, YamlNode)>,
        span: Span,
    },
}

impl YamlNode {
    fn position(&self) -> (u32, u32) {
        let (Self::Scalar { span, .. } | Self::Sequence { span, .. } | Self::Mapping { span, .. }) =
            self;
        (span.start.line() as u32, yaml_column(span.start))
    }
}

/// The one-based column of a marker. `saphyr-parser` columns are zero-based (its docs say otherwise); lines are
/// one-based.
fn yaml_column(marker: saphyr_parser::Marker) -> u32 {
    marker.col() as u32 + 1
}

/// Why a document could not be loaded.
enum ComposeError {
    Syntax {
        line: u32,
        column: u32,
        message: String,
    },
    /// The document nests deeper than the converter accepts; reported like a failed conversion.
    TooDeep,
}

impl From<ScanError> for ComposeError {
    fn from(error: ScanError) -> Self {
        Self::Syntax {
            line: error.marker().line() as u32,
            column: yaml_column(*error.marker()),
            message: error.info().to_owned(),
        }
    }
}

/// Builds YAML node trees from parser events, resolving aliases by copying the anchored node. Anchors are keyed by
/// event id, as libyaml documents number their nodes.
#[doc(alias = "AeronConfig_YamlNodeId")]
struct Composer<'input> {
    parser: Parser<'input, saphyr_parser::StrInput<'input>>,
    anchors: HashMap<usize, YamlNode>,
}

impl<'input> Composer<'input> {
    fn new(text: &'input str) -> Self {
        Self {
            parser: Parser::new_from_str(text),
            anchors: HashMap::new(),
        }
    }

    fn next_event(&mut self) -> Result<(Event<'input>, Span), ComposeError> {
        match self.parser.next_event() {
            Some(result) => Ok(result?),
            None => Ok((Event::StreamEnd, Span::default())),
        }
    }

    /// `yaml_parser_load`: the root node of the next document, or `None` at the end of the stream.
    fn load_document(&mut self) -> Result<Option<YamlNode>, ComposeError> {
        loop {
            match self.next_event()?.0 {
                Event::StreamStart | Event::Nothing => {}
                Event::StreamEnd => return Ok(None),
                Event::DocumentStart(_) => break,
                _ => return Err(unexpected_event()),
            }
        }
        let (event, span) = self.next_event()?;
        let root = self.compose_node(event, span, 0)?;
        match self.next_event()?.0 {
            Event::DocumentEnd => Ok(Some(root)),
            _ => Err(unexpected_event()),
        }
    }

    fn compose_node(
        &mut self,
        event: Event<'input>,
        span: Span,
        depth: usize,
    ) -> Result<YamlNode, ComposeError> {
        if depth > CONFIG_MAX_DEPTH + 1 {
            return Err(ComposeError::TooDeep);
        }
        let (node, anchor) =
            match event {
                Event::Alias(anchor) => {
                    return self.anchors.get(&anchor).cloned().ok_or_else(|| {
                        ComposeError::Syntax {
                            line: span.start.line() as u32,
                            column: yaml_column(span.start),
                            message: "found undefined alias".to_owned(),
                        }
                    });
                }
                Event::Scalar(value, style, anchor, _) => (
                    YamlNode::Scalar {
                        value: value.into_owned(),
                        plain: style == ScalarStyle::Plain,
                        span,
                    },
                    anchor,
                ),
                Event::SequenceStart(anchor, _) => {
                    let mut items = Vec::new();
                    loop {
                        let (event, item_span) = self.next_event()?;
                        if matches!(event, Event::SequenceEnd) {
                            break;
                        }
                        items.push(self.compose_node(event, item_span, depth + 1)?);
                    }
                    (YamlNode::Sequence { items, span }, anchor)
                }
                Event::MappingStart(anchor, _) => {
                    let mut pairs = Vec::new();
                    loop {
                        let (event, key_span) = self.next_event()?;
                        if matches!(event, Event::MappingEnd) {
                            break;
                        }
                        let key = self.compose_node(event, key_span, depth + 1)?;
                        let (event, value_span) = self.next_event()?;
                        let value = self.compose_node(event, value_span, depth + 1)?;
                        pairs.push((key, value));
                    }
                    (YamlNode::Mapping { pairs, span }, anchor)
                }
                _ => return Err(unexpected_event()),
            };
        if anchor != 0 {
            self.anchors.insert(anchor, node.clone());
        }
        Ok(node)
    }
}

fn unexpected_event() -> ComposeError {
    ComposeError::Syntax {
        line: 0,
        column: 0,
        message: "unexpected YAML event".to_owned(),
    }
}

impl ConfigNode {
    /// `AeronConfig_CreateNode` without a YAML source node: position zero.
    #[doc(alias = "AeronConfig_CreateNode")]
    fn new(value: ConfigNodeValue, source_root: VfsRoot, source_path: &Arc<str>) -> Self {
        Self {
            value,
            source_root,
            source_path: Arc::clone(source_path),
            line: 0,
            column: 0,
        }
    }

    #[doc(alias = "AeronConfig_ConvertScalar")]
    fn convert_scalar(value: &str, plain: bool) -> ConfigNodeValue {
        if plain {
            if value.is_empty()
                || equals_ignore_case(value, "null")
                || equals_ignore_case(value, "~")
            {
                return ConfigNodeValue::Null;
            }
            if ["true", "yes", "on"]
                .iter()
                .any(|word| equals_ignore_case(value, word))
            {
                return ConfigNodeValue::Bool(true);
            }
            if ["false", "no", "off"]
                .iter()
                .any(|word| equals_ignore_case(value, word))
            {
                return ConfigNodeValue::Bool(false);
            }
            if let Some(int_value) = parse_int(value) {
                return ConfigNodeValue::Int(int_value);
            }
            if text_looks_float(value)
                && let Some(float_value) = parse_float(value)
            {
                return ConfigNodeValue::Float(float_value);
            }
        }
        ConfigNodeValue::String(value.to_owned())
    }

    /// Converts a composed YAML node. Errors are logged, as upstream does, and reported as `None`.
    #[doc(alias(
        "AeronConfig_ConvertYamlNode",
        "AeronConfig_ConvertSequence",
        "AeronConfig_ConvertMapping"
    ))]
    fn convert_yaml(node: &YamlNode, root: VfsRoot, path: &Arc<str>, depth: usize) -> Option<Self> {
        if depth > CONFIG_MAX_DEPTH {
            tracing::error!(target: "aeron.config", "YAML config nesting exceeds {CONFIG_MAX_DEPTH} levels");
            return None;
        }
        let value = match node {
            YamlNode::Scalar { value, plain, .. } => Self::convert_scalar(value, *plain),
            YamlNode::Sequence { items, .. } => ConfigNodeValue::Sequence(
                items
                    .iter()
                    .map(|item| Self::convert_yaml(item, root, path, depth + 1))
                    .collect::<Option<_>>()?,
            ),
            YamlNode::Mapping { pairs, .. } => {
                let mut entries = Vec::with_capacity(pairs.len());
                for (key, value) in pairs {
                    let YamlNode::Scalar { value: key, .. } = key else {
                        tracing::error!(target: "aeron.config", "YAML config map keys must be scalar values");
                        return None;
                    };
                    entries.push(ConfigMapEntry {
                        key: key.clone(),
                        value: Self::convert_yaml(value, root, path, depth + 1)?,
                    });
                }
                ConfigNodeValue::Map(entries)
            }
        };
        let (line, column) = node.position();
        Some(Self {
            value,
            source_root: root,
            source_path: Arc::clone(path),
            line,
            column,
        })
    }

    /// `AeronConfigNode_FromValue`. Returns `None` for a non-finite float or nesting deeper than 128 levels.
    #[doc(alias = "AeronConfigNode_FromValue")]
    fn from_value(
        value: &ConfigValue,
        root: VfsRoot,
        path: &Arc<str>,
        depth: usize,
    ) -> Option<Self> {
        if depth > CONFIG_MAX_DEPTH {
            return None;
        }
        let node_value = match value {
            ConfigValue::Null => ConfigNodeValue::Null,
            ConfigValue::Bool(value) => ConfigNodeValue::Bool(*value),
            ConfigValue::Int(value) => ConfigNodeValue::Int(*value),
            ConfigValue::Float(value) if value.is_finite() => ConfigNodeValue::Float(*value),
            ConfigValue::Float(_) => return None,
            ConfigValue::String(value) => ConfigNodeValue::String(value.clone()),
            ConfigValue::Map(entries) => ConfigNodeValue::Map(
                entries
                    .iter()
                    .map(|(key, value)| {
                        Some(ConfigMapEntry {
                            key: key.clone(),
                            value: Self::from_value(value, root, path, depth + 1)?,
                        })
                    })
                    .collect::<Option<_>>()?,
            ),
            ConfigValue::Sequence(items) => ConfigNodeValue::Sequence(
                items
                    .iter()
                    .map(|item| Self::from_value(item, root, path, depth + 1))
                    .collect::<Option<_>>()?,
            ),
        };
        Some(Self::new(node_value, root, path))
    }

    /// The node's value.
    pub fn value(&self) -> &ConfigNodeValue {
        &self.value
    }

    #[doc(alias = "AeronConfigNode_Type")]
    pub fn node_type(&self) -> ConfigNodeType {
        match self.value {
            ConfigNodeValue::Null => ConfigNodeType::Null,
            ConfigNodeValue::Bool(_) => ConfigNodeType::Bool,
            ConfigNodeValue::Int(_) => ConfigNodeType::Int,
            ConfigNodeValue::Float(_) => ConfigNodeType::Float,
            ConfigNodeValue::String(_) => ConfigNodeType::String,
            ConfigNodeValue::Map(_) => ConfigNodeType::Map,
            ConfigNodeValue::Sequence(_) => ConfigNodeType::Sequence,
        }
    }

    /// One-based YAML line, or zero for nodes created in code.
    #[doc(alias = "AeronConfigNode_Line")]
    pub fn line(&self) -> u32 {
        self.line
    }

    /// One-based YAML column, or zero for nodes created in code.
    #[doc(alias = "AeronConfigNode_Column")]
    pub fn column(&self) -> u32 {
        self.column
    }

    #[doc(alias = "AeronConfigNode_SourceRoot")]
    pub fn source_root(&self) -> VfsRoot {
        self.source_root
    }

    #[doc(alias = "AeronConfigNode_SourcePath")]
    pub fn source_path(&self) -> &str {
        &self.source_path
    }

    /// The string value, or `None` for other types.
    #[doc(alias = "AeronConfigNode_String")]
    pub fn as_str(&self) -> Option<&str> {
        match &self.value {
            ConfigNodeValue::String(value) => Some(value),
            _ => None,
        }
    }

    /// The bool value, or `None` for other types.
    #[doc(alias = "AeronConfigNode_Bool")]
    pub fn as_bool(&self) -> Option<bool> {
        match self.value {
            ConfigNodeValue::Bool(value) => Some(value),
            _ => None,
        }
    }

    /// The int value, or `None` for other types.
    #[doc(alias = "AeronConfigNode_Int")]
    pub fn as_int(&self) -> Option<i64> {
        match self.value {
            ConfigNodeValue::Int(value) => Some(value),
            _ => None,
        }
    }

    /// The float value, or an int value converted to `f64`; `None` for other types.
    #[doc(alias = "AeronConfigNode_Float")]
    pub fn as_float(&self) -> Option<f64> {
        match self.value {
            ConfigNodeValue::Float(value) => Some(value),
            ConfigNodeValue::Int(value) => Some(value as f64),
            _ => None,
        }
    }

    fn map_entries(&self) -> &[ConfigMapEntry] {
        match &self.value {
            ConfigNodeValue::Map(entries) => entries,
            _ => &[],
        }
    }

    /// Number of map entries; zero for non-map nodes.
    #[doc(alias = "AeronConfigNode_MapCount")]
    pub fn map_count(&self) -> usize {
        self.map_entries().len()
    }

    #[doc(alias = "AeronConfigNode_MapKeyAt")]
    pub fn map_key_at(&self, index: usize) -> Option<&str> {
        self.map_entries()
            .get(index)
            .map(|entry| entry.key.as_str())
    }

    #[doc(alias = "AeronConfigNode_MapValueAt")]
    pub fn map_value_at(&self, index: usize) -> Option<&ConfigNode> {
        self.map_entries().get(index).map(|entry| &entry.value)
    }

    /// The value of the first entry with `key`; `None` when missing or not a map.
    #[doc(alias("AeronConfigNode_MapGet", "AeronConfigNode_MapGetLength"))]
    pub fn map_get(&self, key: &str) -> Option<&ConfigNode> {
        self.map_entries()
            .iter()
            .find(|entry| entry.key == key)
            .map(|entry| &entry.value)
    }

    fn sequence_items(&self) -> &[ConfigNode] {
        match &self.value {
            ConfigNodeValue::Sequence(items) => items,
            _ => &[],
        }
    }

    /// Number of sequence items; zero for non-sequence nodes.
    #[doc(alias = "AeronConfigNode_SequenceCount")]
    pub fn sequence_count(&self) -> usize {
        self.sequence_items().len()
    }

    #[doc(alias = "AeronConfigNode_SequenceGet")]
    pub fn sequence_get(&self, index: usize) -> Option<&ConfigNode> {
        self.sequence_items().get(index)
    }

    /// Resolves a dotted/indexed path such as `video.modes[0]`. An empty path is the node itself.
    #[doc(alias = "AeronConfigNode_PathGet")]
    pub fn path_get(&self, path: &str) -> Option<&ConfigNode> {
        let bytes = path.as_bytes();
        let mut node = self;
        let mut p = 0;
        while p < bytes.len() {
            if bytes[p] == b'.' {
                return None;
            }
            let key_start = p;
            while p < bytes.len() && bytes[p] != b'.' && bytes[p] != b'[' {
                p += 1;
            }
            if p > key_start {
                node = node.map_get(&path[key_start..p])?;
            }
            while bytes.get(p) == Some(&b'[') {
                p += 1;
                if !bytes.get(p).is_some_and(u8::is_ascii_digit) {
                    return None;
                }
                let mut index = 0usize;
                while let Some(&digit) = bytes.get(p).filter(|digit| digit.is_ascii_digit()) {
                    index = index
                        .checked_mul(10)?
                        .checked_add(usize::from(digit - b'0'))?;
                    p += 1;
                }
                if bytes.get(p) != Some(&b']') {
                    return None;
                }
                p += 1;
                node = node.sequence_get(index)?;
            }
            match bytes.get(p) {
                Some(b'.') => {
                    p += 1;
                    if p == bytes.len() {
                        return None;
                    }
                }
                Some(_) => return None,
                None => {}
            }
        }
        Some(node)
    }

    /// Sets the source of this node and all its children.
    #[doc(alias = "AeronConfigNode_SetSource")]
    fn set_source(&mut self, root: VfsRoot, path: &Arc<str>) {
        self.source_root = root;
        self.source_path = Arc::clone(path);
        match &mut self.value {
            ConfigNodeValue::Map(entries) => {
                for entry in entries {
                    entry.value.set_source(root, path);
                }
            }
            ConfigNodeValue::Sequence(items) => {
                for item in items {
                    item.set_source(root, path);
                }
            }
            _ => {}
        }
    }

    /// Merges `overrides` into a copy of `self`: maps merge key by key, anything else is replaced.
    #[doc(alias = "AeronConfigNode_Overlay")]
    fn overlay(&self, overrides: &ConfigNode) -> ConfigNode {
        let (ConfigNodeValue::Map(base_entries), ConfigNodeValue::Map(override_entries)) =
            (&self.value, &overrides.value)
        else {
            return overrides.clone();
        };
        let mut entries = base_entries.clone();
        for entry in override_entries {
            match entries
                .iter()
                .position(|existing| existing.key == entry.key)
            {
                Some(target) => entries[target].value = entries[target].value.overlay(&entry.value),
                None => entries.push(entry.clone()),
            }
        }
        ConfigNode {
            value: ConfigNodeValue::Map(entries),
            ..self.clone_metadata()
        }
    }

    /// A null node with this node's source and position.
    fn clone_metadata(&self) -> ConfigNode {
        ConfigNode {
            value: ConfigNodeValue::Null,
            source_root: self.source_root,
            source_path: Arc::clone(&self.source_path),
            line: self.line,
            column: self.column,
        }
    }

    /// Removes the dotted `path` below this map. Returns whether this map is empty afterwards; a parent then
    /// removes it too.
    #[doc(alias = "AeronConfigNode_Remove")]
    fn remove(&mut self, path: &str) -> Result<bool, ConfigError> {
        let (segment, rest) = match path.split_once('.') {
            Some((segment, rest)) => (segment, Some(rest)),
            None => (path, None),
        };
        // Callers only pass maps: the document root is checked, and children are checked below.
        let ConfigNodeValue::Map(entries) = &mut self.value else {
            return Ok(false);
        };
        let Some(index) = entries.iter().position(|entry| entry.key == segment) else {
            return Ok(entries.is_empty());
        };
        if let Some(rest) = rest {
            let child = &mut entries[index].value;
            if child.node_type() != ConfigNodeType::Map {
                return fail(
                    ConfigErrorCode::PathTypeConflict,
                    child.source_root,
                    &child.source_path,
                    child.line,
                    child.column,
                    "configuration path crosses a non-map value",
                );
            }
            if !child.remove(rest)? {
                return Ok(false);
            }
        }
        entries.remove(index);
        Ok(entries.is_empty())
    }
}

#[doc(alias = "AeronConfig_ValidMutationPath")]
fn valid_mutation_path(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with('.')
        && !path.contains('[')
        && !path.ends_with('.')
        && !path.contains("..")
}

/// `AeronConfigBuffer_Quoted`: a double-quoted YAML string.
#[doc(alias = "AeronConfigBuffer_Quoted")]
fn emit_quoted(buffer: &mut String, text: &str) {
    buffer.push('"');
    for c in text.chars() {
        match c {
            '\\' => buffer.push_str("\\\\"),
            '"' => buffer.push_str("\\\""),
            '\n' => buffer.push_str("\\n"),
            '\r' => buffer.push_str("\\r"),
            '\t' => buffer.push_str("\\t"),
            '\0'..='\x1f' | '\x7f' => buffer.push_str(&format!("\\u{:04x}", u32::from(c))),
            _ => buffer.push(c),
        }
    }
    buffer.push('"');
}

#[doc(alias = "AeronConfig_KeyIsPlain")]
fn key_is_plain(key: &str) -> bool {
    let mut bytes = key.bytes();
    bytes
        .next()
        .is_some_and(|first| first == b'_' || first.is_ascii_alphabetic())
        && bytes.all(|byte| byte == b'_' || byte == b'-' || byte.is_ascii_alphanumeric())
}

/// The shortest `%.*g` text that parses back to exactly `value`, always with a `.`.
#[doc(alias = "AeronConfig_FormatRoundTripFloat")]
fn format_round_trip_float(value: f64) -> Option<String> {
    let mut candidate = (1..=17).find_map(|precision| {
        let text = format_ascii_double(value, precision)?;
        let parsed = parse_ascii_double(&text)?;
        (parsed.to_bits() == value.to_bits()).then_some(text)
    })?;
    if !candidate.contains('.') {
        let offset = candidate.find(['e', 'E']).unwrap_or(candidate.len());
        candidate.insert_str(offset, ".0");
    }
    Some(candidate)
}

#[doc(alias = "AeronConfig_EmitScalar")]
fn emit_scalar(buffer: &mut String, node: &ConfigNode) -> bool {
    match &node.value {
        ConfigNodeValue::Null => buffer.push_str("null"),
        ConfigNodeValue::Bool(value) => buffer.push_str(if *value { "true" } else { "false" }),
        ConfigNodeValue::Int(value) => buffer.push_str(&value.to_string()),
        ConfigNodeValue::Float(value) => match format_round_trip_float(*value) {
            Some(text) => buffer.push_str(&text),
            None => return false,
        },
        ConfigNodeValue::String(value) => emit_quoted(buffer, value),
        ConfigNodeValue::Map(_) | ConfigNodeValue::Sequence(_) => return false,
    }
    true
}

fn is_compound(node: &ConfigNode) -> bool {
    matches!(
        node.value,
        ConfigNodeValue::Map(_) | ConfigNodeValue::Sequence(_)
    )
}

#[doc(alias = "AeronConfigBuffer_Indent")]
fn emit_indent(buffer: &mut String, indent: usize) {
    buffer.extend(std::iter::repeat_n(' ', indent));
}

/// `buffer` is a `String`, which covers `AeronConfigBuffer_Reserve`, `AeronConfigBuffer_Append` and
/// `AeronConfigBuffer_Text`.
#[doc(alias(
    "AeronConfig_EmitNode",
    "AeronConfig_EmitMap",
    "AeronConfig_EmitSequence",
    "AeronConfigBuffer_Reserve",
    "AeronConfigBuffer_Append",
    "AeronConfigBuffer_Text"
))]
fn emit_node(buffer: &mut String, node: &ConfigNode, indent: usize) -> bool {
    match &node.value {
        ConfigNodeValue::Map(entries) if entries.is_empty() => {
            emit_indent(buffer, indent);
            buffer.push_str("{}\n");
        }
        ConfigNodeValue::Map(entries) => {
            for entry in entries {
                emit_indent(buffer, indent);
                if key_is_plain(&entry.key) {
                    buffer.push_str(&entry.key);
                } else {
                    emit_quoted(buffer, &entry.key);
                }
                if is_compound(&entry.value) {
                    buffer.push_str(":\n");
                    if !emit_node(buffer, &entry.value, indent + 2) {
                        return false;
                    }
                } else {
                    buffer.push_str(": ");
                    if !emit_scalar(buffer, &entry.value) {
                        return false;
                    }
                    buffer.push('\n');
                }
            }
        }
        ConfigNodeValue::Sequence(items) if items.is_empty() => {
            emit_indent(buffer, indent);
            buffer.push_str("[]\n");
        }
        ConfigNodeValue::Sequence(items) => {
            for item in items {
                emit_indent(buffer, indent);
                if is_compound(item) {
                    buffer.push_str("-\n");
                    if !emit_node(buffer, item, indent + 2) {
                        return false;
                    }
                } else {
                    buffer.push_str("- ");
                    if !emit_scalar(buffer, item) {
                        return false;
                    }
                    buffer.push('\n');
                }
            }
        }
        _ => {
            emit_indent(buffer, indent);
            if !emit_scalar(buffer, node) {
                return false;
            }
            buffer.push('\n');
        }
    }
    true
}

/// The one-based line and column of a byte offset.
fn text_position(text: &[u8], offset: usize) -> (u32, u32) {
    let before = &text[..offset];
    let line = before.iter().filter(|&&byte| byte == b'\n').count() + 1;
    let column = offset
        - before
            .iter()
            .rposition(|&byte| byte == b'\n')
            .map_or(0, |newline| newline + 1)
        + 1;
    (line as u32, column as u32)
}

impl ConfigFile {
    fn error<T>(
        &self,
        code: ConfigErrorCode,
        line: u32,
        column: u32,
        message: &str,
    ) -> Result<T, ConfigError> {
        fail(code, self.root_kind, &self.path, line, column, message)
    }

    /// Loads a single-document YAML file and converts it to nodes.
    #[doc(alias = "AeronConfigFile_LoadYamlEx")]
    pub fn load_yaml_ex(
        vfs: &mut Vfs,
        root: VfsRoot,
        path: &str,
    ) -> Result<ConfigFile, ConfigError> {
        let error =
            |code, line, column, message: &str| fail(code, root, path, line, column, message);
        if path.is_empty() {
            return error(
                ConfigErrorCode::InvalidArgument,
                0,
                0,
                "invalid configuration load arguments",
            );
        }
        match vfs.stat(root, path) {
            Ok(info) if info.exists && info.is_directory => {
                return error(
                    ConfigErrorCode::Io,
                    0,
                    0,
                    "configuration path is a directory",
                );
            }
            Ok(info) if info.exists => {}
            _ => {
                return error(
                    ConfigErrorCode::NotFound,
                    0,
                    0,
                    "configuration file not found",
                );
            }
        }
        let Some(data) = read_file(vfs, root, path) else {
            return error(
                ConfigErrorCode::Io,
                0,
                0,
                "could not read configuration file",
            );
        };
        let text = match std::str::from_utf8(&data) {
            Ok(text) => text.strip_prefix('\u{feff}').unwrap_or(text),
            Err(utf8) => {
                let (line, column) = text_position(&data, utf8.valid_up_to());
                return error(
                    ConfigErrorCode::YamlSyntax,
                    line,
                    column,
                    "invalid UTF-8 octet",
                );
            }
        };

        let mut composer = Composer::new(text);
        let document = match composer.load_document() {
            Ok(document) => document,
            Err(ComposeError::Syntax {
                line,
                column,
                message,
            }) => {
                return error(ConfigErrorCode::YamlSyntax, line, column, &message);
            }
            Err(ComposeError::TooDeep) => {
                tracing::error!(target: "aeron.config", "YAML config nesting exceeds {CONFIG_MAX_DEPTH} levels");
                return error(
                    ConfigErrorCode::OutOfMemory,
                    0,
                    0,
                    "could not convert YAML document",
                );
            }
        };

        let source: Arc<str> = Arc::from(path);
        let converted = match &document {
            Some(node) => ConfigNode::convert_yaml(node, root, &source, 0),
            None => Some(ConfigNode::new(ConfigNodeValue::Null, root, &source)),
        };
        let Some(mut root_node) = converted else {
            return error(
                ConfigErrorCode::OutOfMemory,
                0,
                0,
                "could not convert YAML document",
            );
        };
        root_node.set_source(root, &source);

        match composer.load_document() {
            Ok(None) => {}
            Ok(Some(_)) => {
                return error(
                    ConfigErrorCode::MultipleDocuments,
                    0,
                    0,
                    "configuration must contain one YAML document",
                );
            }
            Err(ComposeError::Syntax {
                line,
                column,
                message,
            }) => {
                return error(ConfigErrorCode::YamlSyntax, line, column, &message);
            }
            // A second document exists, however deep it nests.
            Err(ComposeError::TooDeep) => {
                return error(
                    ConfigErrorCode::MultipleDocuments,
                    0,
                    0,
                    "configuration must contain one YAML document",
                );
            }
        }
        Ok(ConfigFile {
            root: root_node,
            root_kind: root,
            path: path.to_owned(),
        })
    }

    /// Like [`ConfigFile::load_yaml_ex`], logging every failure except a missing file.
    #[doc(alias = "AeronConfigFile_LoadYaml")]
    pub fn load_yaml(vfs: &mut Vfs, root: VfsRoot, path: &str) -> Option<ConfigFile> {
        match Self::load_yaml_ex(vfs, root, path) {
            Ok(config) => Some(config),
            Err(error) => {
                if error.code != ConfigErrorCode::NotFound {
                    tracing::error!(target: "aeron.config", "{error}");
                }
                None
            }
        }
    }

    /// A document holding an empty map, to be saved at `root`/`path`.
    #[doc(alias("AeronConfigFile_CreateMap", "AeronConfigFile_Allocate"))]
    pub fn create_map(root: VfsRoot, path: &str) -> ConfigFile {
        let source: Arc<str> = Arc::from(path);
        ConfigFile {
            root: ConfigNode::new(ConfigNodeValue::Map(Vec::new()), root, &source),
            root_kind: root,
            path: path.to_owned(),
        }
    }

    /// `base` with `overrides` merged in: maps merge key by key, anything else is replaced. The result keeps the
    /// root and path of `base`.
    #[doc(alias = "AeronConfigFile_Overlay")]
    pub fn overlay(base: &ConfigFile, overrides: &ConfigFile) -> ConfigFile {
        ConfigFile {
            root: base.root.overlay(&overrides.root),
            root_kind: base.root_kind,
            path: base.path.clone(),
        }
    }

    /// Serialises the document as block-style YAML.
    #[doc(alias("AeronConfigFile_SerializeYaml", "AeronConfigFile_FreeSerialized"))]
    pub fn serialize_yaml(&self) -> Result<String, ConfigError> {
        let mut buffer = String::new();
        if !emit_node(&mut buffer, &self.root, 0) {
            return self.error(
                ConfigErrorCode::OutOfMemory,
                0,
                0,
                "could not serialize configuration document",
            );
        }
        Ok(buffer)
    }

    /// Serialises the document and atomically writes it to its root and path.
    #[doc(alias = "AeronConfigFile_SaveYaml")]
    pub fn save_yaml(&self, vfs: &mut Vfs) -> Result<(), ConfigError> {
        if self.root_kind == VfsRoot::Resource {
            return self.error(
                ConfigErrorCode::InvalidArgument,
                0,
                0,
                "invalid configuration save arguments",
            );
        }
        let data = self.serialize_yaml()?;
        if vfs
            .write_all_atomic(self.root_kind, &self.path, data.as_bytes())
            .is_err()
        {
            return self.error(
                ConfigErrorCode::Io,
                0,
                0,
                "Could not save your settings. Try again.",
            );
        }
        Ok(())
    }

    #[doc(alias(
        "AeronConfigFile_SetCloned",
        "AeronConfigNode_MapFind",
        "AeronConfigNode_MapAppend"
    ))]
    fn set_in(&mut self, path: &str, value: &ConfigValue) -> Result<(), ConfigError> {
        let root_kind = self.root_kind;
        let source: Arc<str> = Arc::from(self.path.as_str());
        let mut map = &mut self.root;
        let mut conflict = "configuration root is not a map";
        let mut segments = path.split('.').peekable();
        while let Some(segment) = segments.next() {
            let (line, column) = (map.line, map.column);
            let ConfigNodeValue::Map(entries) = &mut map.value else {
                return fail(
                    ConfigErrorCode::PathTypeConflict,
                    root_kind,
                    &source,
                    line,
                    column,
                    conflict,
                );
            };
            conflict = "configuration path crosses a non-map value";
            let index = entries.iter().position(|entry| entry.key == segment);
            if segments.peek().is_none() {
                let Some(replacement) = ConfigNode::from_value(value, root_kind, &source, 0) else {
                    return fail(
                        ConfigErrorCode::OutOfMemory,
                        root_kind,
                        &source,
                        0,
                        0,
                        "could not allocate configuration value",
                    );
                };
                match index {
                    Some(index) => entries[index].value = replacement,
                    None => entries.push(ConfigMapEntry {
                        key: segment.to_owned(),
                        value: replacement,
                    }),
                }
                return Ok(());
            }
            let index = index.unwrap_or_else(|| {
                entries.push(ConfigMapEntry {
                    key: segment.to_owned(),
                    value: ConfigNode::new(ConfigNodeValue::Map(Vec::new()), root_kind, &source),
                });
                entries.len() - 1
            });
            map = &mut entries[index].value;
        }
        Ok(())
    }

    /// Sets the value at a dotted path (no `[index]` parts), creating missing intermediate maps. The document is
    /// unchanged on failure.
    #[doc(alias = "AeronConfigFile_SetValue")]
    pub fn set_value(&mut self, path: &str, value: &ConfigValue) -> Result<(), ConfigError> {
        if !valid_mutation_path(path) {
            return self.error(
                ConfigErrorCode::InvalidArgument,
                0,
                0,
                "invalid configuration mutation arguments",
            );
        }
        let mut clone = self.clone();
        clone.set_in(path, value)?;
        self.root = clone.root;
        Ok(())
    }

    #[doc(alias = "AeronConfigFile_SetBool")]
    pub fn set_bool(&mut self, path: &str, value: bool) -> Result<(), ConfigError> {
        self.set_value(path, &ConfigValue::Bool(value))
    }

    #[doc(alias = "AeronConfigFile_SetInt")]
    pub fn set_int(&mut self, path: &str, value: i64) -> Result<(), ConfigError> {
        self.set_value(path, &ConfigValue::Int(value))
    }

    /// Sets a finite float.
    #[doc(alias = "AeronConfigFile_SetFloat")]
    pub fn set_float(&mut self, path: &str, value: f64) -> Result<(), ConfigError> {
        if !value.is_finite() {
            return self.error(
                ConfigErrorCode::InvalidArgument,
                0,
                0,
                "configuration float must be finite",
            );
        }
        self.set_value(path, &ConfigValue::Float(value))
    }

    #[doc(alias = "AeronConfigFile_SetString")]
    pub fn set_string(&mut self, path: &str, value: &str) -> Result<(), ConfigError> {
        self.set_value(path, &ConfigValue::String(value.to_owned()))
    }

    /// Removes the value at a dotted path, then every map left empty along it. A missing path is not an error.
    #[doc(alias = "AeronConfigFile_Remove")]
    pub fn remove(&mut self, path: &str) -> Result<(), ConfigError> {
        if !valid_mutation_path(path) || self.root.node_type() != ConfigNodeType::Map {
            return self.error(
                ConfigErrorCode::InvalidArgument,
                0,
                0,
                "invalid configuration removal arguments",
            );
        }
        let mut root = self.root.clone();
        root.remove(path)?;
        self.root = root;
        Ok(())
    }

    #[doc(alias = "AeronConfigFile_Root")]
    pub fn root(&self) -> &ConfigNode {
        &self.root
    }

    /// The VFS root the document loads from and saves to.
    pub fn root_kind(&self) -> VfsRoot {
        self.root_kind
    }

    /// The path the document loads from and saves to.
    pub fn path(&self) -> &str {
        &self.path
    }

    /// Resolves a dotted/indexed path such as `video.modes[0]` from the root node.
    #[doc(alias = "AeronConfigFile_GetNode")]
    pub fn get_node(&self, path: &str) -> Option<&ConfigNode> {
        self.root.path_get(path)
    }

    #[doc(alias = "AeronConfigFile_Has")]
    pub fn has(&self, path: &str) -> bool {
        self.get_node(path).is_some()
    }

    #[doc(alias = "AeronConfigFile_GetString")]
    pub fn get_string(&self, path: &str) -> Option<&str> {
        self.get_node(path)?.as_str()
    }

    #[doc(alias = "AeronConfigFile_GetBool")]
    pub fn get_bool(&self, path: &str) -> Option<bool> {
        self.get_node(path)?.as_bool()
    }

    #[doc(alias = "AeronConfigFile_GetInt")]
    pub fn get_int(&self, path: &str) -> Option<i64> {
        self.get_node(path)?.as_int()
    }

    /// A float, or an int converted to `f64`.
    #[doc(alias = "AeronConfigFile_GetFloat")]
    pub fn get_float(&self, path: &str) -> Option<f64> {
        self.get_node(path)?.as_float()
    }
}

#[doc(alias = "AeronConfig_ReadFile")]
fn read_file(vfs: &mut Vfs, root: VfsRoot, path: &str) -> Option<Vec<u8>> {
    let mut file = vfs.open(root, path, OpenMode::Read).ok()?;
    let size = usize::try_from(file.size()?).ok()?;
    let mut data = vec![0; size];
    (file.read(&mut data) == size).then_some(data)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vfs::VfsConfig;

    struct Fixture {
        _dir: tempfile::TempDir,
        vfs: Vfs,
    }

    fn fixture(files: &[(&str, &str)]) -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_str().unwrap().to_owned();
        for (name, text) in files {
            std::fs::write(format!("{root}/{name}"), text).unwrap();
        }
        let vfs = Vfs::new(&VfsConfig {
            asset_root: Some(root.clone()),
            resource_root: Some(root.clone()),
            user_root: Some(root),
            ..VfsConfig::default()
        });
        Fixture { _dir: dir, vfs }
    }

    fn load(text: &str) -> Result<ConfigFile, ConfigError> {
        let mut fixture = fixture(&[("config.yaml", text)]);
        ConfigFile::load_yaml_ex(&mut fixture.vfs, VfsRoot::User, "config.yaml")
    }

    #[test]
    fn scalar_typing() {
        let config = load(
            "a: ~\nb: Yes\nc: off\nd: 0x1F\ne: 010\nf: -12\ng: 1.5\nh: 1e3\ni: 08\nj: '12'\nk: \"true\"\n\
             l: 9223372036854775808\nm: .inf\nn:\no: 1_000\n",
        )
        .unwrap();
        let value = |key: &str| config.get_node(key).unwrap().value().clone();
        assert_eq!(value("a"), ConfigNodeValue::Null);
        assert_eq!(value("b"), ConfigNodeValue::Bool(true));
        assert_eq!(value("c"), ConfigNodeValue::Bool(false));
        assert_eq!(value("d"), ConfigNodeValue::Int(31));
        assert_eq!(value("e"), ConfigNodeValue::Int(8));
        assert_eq!(value("f"), ConfigNodeValue::Int(-12));
        assert_eq!(value("g"), ConfigNodeValue::Float(1.5));
        assert_eq!(value("h"), ConfigNodeValue::Float(1000.0));
        assert_eq!(value("i"), ConfigNodeValue::String("08".into()));
        assert_eq!(value("j"), ConfigNodeValue::String("12".into()));
        assert_eq!(value("k"), ConfigNodeValue::String("true".into()));
        assert_eq!(
            value("l"),
            ConfigNodeValue::String("9223372036854775808".into())
        );
        assert_eq!(value("m"), ConfigNodeValue::String(".inf".into()));
        assert_eq!(value("n"), ConfigNodeValue::Null);
        assert_eq!(value("o"), ConfigNodeValue::String("1_000".into()));
        assert_eq!(parse_int("-9223372036854775808"), Some(i64::MIN));
        assert_eq!(parse_int("0x"), None);
    }

    #[test]
    fn paths_and_accessors() {
        let config =
            load("video:\n  modes:\n    - [640, 480]\n    - {w: 800}\n  scale: 2\nname: XW\n")
                .unwrap();
        assert_eq!(config.get_int("video.modes[0][1]"), Some(480));
        assert_eq!(config.get_int("video.modes[1].w"), Some(800));
        assert_eq!(config.get_float("video.scale"), Some(2.0));
        assert_eq!(config.get_string("name"), Some("XW"));
        assert_eq!(config.get_string("video.scale"), None);
        for invalid in [
            ".video",
            "video.",
            "video..scale",
            "video.modes[",
            "video.modes[x]",
            "video.modes[9]",
        ] {
            assert!(!config.has(invalid), "{invalid}");
        }
        assert!(config.has(""));
        let video = config.get_node("video").unwrap();
        assert_eq!(video.map_count(), 2);
        assert_eq!(video.map_key_at(1), Some("scale"));
        assert_eq!(video.map_value_at(1).and_then(ConfigNode::as_int), Some(2));
        assert_eq!(video.map_get("modes").unwrap().sequence_count(), 2);
        let scale = config.get_node("video.scale").unwrap();
        assert_eq!((scale.line(), scale.column()), (5, 10));
        assert_eq!(scale.source_path(), "config.yaml");
        assert_eq!(scale.source_root(), VfsRoot::User);
    }

    #[test]
    fn load_errors() {
        let mut fixture = fixture(&[
            ("bad.yaml", "a: [1, 2\n"),
            ("two.yaml", "a: 1\n---\nb: 2\n"),
        ]);
        let vfs = &mut fixture.vfs;
        let code = |vfs: &mut Vfs, path| {
            ConfigFile::load_yaml_ex(vfs, VfsRoot::User, path)
                .unwrap_err()
                .code
        };
        assert_eq!(code(vfs, "missing.yaml"), ConfigErrorCode::NotFound);
        assert_eq!(code(vfs, "bad.yaml"), ConfigErrorCode::YamlSyntax);
        assert_eq!(code(vfs, "two.yaml"), ConfigErrorCode::MultipleDocuments);
        assert_eq!(code(vfs, "."), ConfigErrorCode::Io);
        assert_eq!(code(vfs, ""), ConfigErrorCode::InvalidArgument);
        assert!(ConfigFile::load_yaml(vfs, VfsRoot::User, "missing.yaml").is_none());

        assert_eq!(
            load("? [a]\n: 1\n").unwrap_err().code,
            ConfigErrorCode::OutOfMemory
        );
        assert_eq!(
            load("a: *undefined\n").unwrap_err().code,
            ConfigErrorCode::YamlSyntax
        );
        let deep = format!("{}{}", "[".repeat(200), "]".repeat(200));
        assert_eq!(load(&deep).unwrap_err().code, ConfigErrorCode::OutOfMemory);
    }

    #[test]
    fn empty_documents_and_aliases() {
        assert_eq!(load("").unwrap().root().node_type(), ConfigNodeType::Null);
        assert_eq!(
            load("# comment\n").unwrap().root().node_type(),
            ConfigNodeType::Null
        );
        let config = load("base: &b {x: 1}\ncopy: *b\n").unwrap();
        assert_eq!(config.get_int("copy.x"), Some(1));
    }

    #[test]
    fn mutation() {
        let mut config = ConfigFile::create_map(VfsRoot::User, "settings.yaml");
        config.set_int("video.width", 640).unwrap();
        config.set_bool("video.fullscreen", true).unwrap();
        config.set_string("name", "Luke").unwrap();
        config.set_int("video.width", 800).unwrap();
        assert_eq!(config.get_int("video.width"), Some(800));
        assert_eq!(config.root().map_key_at(0), Some("video"));

        let before = config.clone();
        let error = config.set_int("name.first", 1).unwrap_err();
        assert_eq!(error.code, ConfigErrorCode::PathTypeConflict);
        assert_eq!(
            config.set_float("x", f64::NAN).unwrap_err().code,
            ConfigErrorCode::InvalidArgument
        );
        let nested_nan = ConfigValue::Sequence(vec![ConfigValue::Float(f64::INFINITY)]);
        assert_eq!(
            config.set_value("y.z", &nested_nan).unwrap_err().code,
            ConfigErrorCode::OutOfMemory
        );
        for invalid in ["", ".a", "a.", "a..b", "a[0]"] {
            assert_eq!(
                config.set_int(invalid, 0).unwrap_err().code,
                ConfigErrorCode::InvalidArgument
            );
        }
        assert_eq!(config, before);

        config
            .set_value(
                "audio",
                &ConfigValue::Map(vec![("volume".into(), ConfigValue::Float(0.5))]),
            )
            .unwrap();
        assert_eq!(config.get_float("audio.volume"), Some(0.5));
        assert_eq!(config.get_node("audio.volume").unwrap().line(), 0);

        // Removing the last key of a map removes the map too.
        config.remove("audio.volume").unwrap();
        assert!(!config.has("audio"));
        config.remove("video.missing").unwrap();
        assert!(config.has("video.width"));
        assert_eq!(
            config.remove("name.first").unwrap_err().code,
            ConfigErrorCode::PathTypeConflict
        );
        config
            .set_value("empty", &ConfigValue::Map(Vec::new()))
            .unwrap();
        config.remove("empty.missing").unwrap();
        assert!(!config.has("empty"));
    }

    #[test]
    fn overlay_merges_maps() {
        let base = load("a: {x: 1, y: 2}\nb: [1]\nc: 3\n").unwrap();
        let mut fixture = fixture(&[("o.yaml", "a: {y: 20, z: 30}\nb: [2, 3]\nd: 4\n")]);
        let overrides =
            ConfigFile::load_yaml_ex(&mut fixture.vfs, VfsRoot::Asset, "o.yaml").unwrap();
        let merged = ConfigFile::overlay(&base, &overrides);
        assert_eq!(merged.get_int("a.x"), Some(1));
        assert_eq!(merged.get_int("a.y"), Some(20));
        assert_eq!(merged.get_int("a.z"), Some(30));
        assert_eq!(merged.get_int("b[1]"), Some(3));
        assert_eq!(merged.get_int("c"), Some(3));
        assert_eq!(merged.get_node("d").unwrap().source_root(), VfsRoot::Asset);
        assert_eq!(merged.path(), "config.yaml");
    }

    #[test]
    fn serialization_round_trip() {
        let mut config = ConfigFile::create_map(VfsRoot::User, "out.yaml");
        config.set_float("f.one", 1.0).unwrap();
        config.set_float("f.tenth", 0.1).unwrap();
        config.set_float("f.big", 1e20).unwrap();
        config.set_float("f.negative_zero", -0.0).unwrap();
        config.set_string("s.quoted", "a\"b\\c\n\u{1}é").unwrap();
        config
            .set_value("s.\"odd key\"", &ConfigValue::Null)
            .unwrap();
        config
            .set_value(
                "list",
                &ConfigValue::Sequence(vec![
                    ConfigValue::Int(-5),
                    ConfigValue::Map(vec![("k".into(), ConfigValue::Bool(false))]),
                    ConfigValue::Sequence(Vec::new()),
                    ConfigValue::Map(Vec::new()),
                ]),
            )
            .unwrap();
        let text = config.serialize_yaml().unwrap();
        assert_eq!(
            text,
            "f:\n  one: 1.0\n  tenth: 0.1\n  big: 1.0e+20\n  negative_zero: -0.0\n\
             s:\n  quoted: \"a\\\"b\\\\c\\n\\u0001é\"\n  \"\\\"odd key\\\"\": null\n\
             list:\n  - -5\n  -\n    k: false\n  -\n    []\n  -\n    {}\n"
        );

        let mut fixture = fixture(&[]);
        config.save_yaml(&mut fixture.vfs).unwrap();
        let loaded = ConfigFile::load_yaml_ex(&mut fixture.vfs, VfsRoot::User, "out.yaml").unwrap();
        assert_eq!(loaded.get_float("f.big"), Some(1e20));
        assert_eq!(loaded.get_string("s.quoted"), Some("a\"b\\c\n\u{1}é"));
        assert_eq!(loaded.get_node("list[3]").unwrap().map_count(), 0);
        assert_eq!(loaded.serialize_yaml().unwrap(), text);

        let resource = ConfigFile::create_map(VfsRoot::Resource, "x.yaml");
        assert_eq!(
            resource.save_yaml(&mut fixture.vfs).unwrap_err().code,
            ConfigErrorCode::InvalidArgument
        );
    }
}
