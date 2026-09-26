//! Weighted greedy consensus for rooted binary phylogenetic trees.
//!
//! The command line reads a JSON document from a path or standard input and
//! writes the consensus result to standard output.  The implementation is
//! intentionally dependency-free so the small service binary can be built in a
//! minimal Rust environment.

use std::collections::HashSet;
use std::fs;
use std::io::Read;
use std::process;

/* ----------------------------- JSON data model ---------------------------- */

/// A small JSON subset implementation sufficient for this tool's input and
/// output.  Objects preserve insertion order.
#[derive(Debug, Clone)]
pub enum Json {
    Null,
    Bool(bool),
    Integer(i64),
    Number(f64),
    String(String),
    Array(Vec<Json>),
    Object(Vec<(String, Json)>),
}

impl Json {
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Json::String(value) => Some(value),
            _ => None,
        }
    }

    pub fn as_array(&self) -> Option<&Vec<Json>> {
        match self {
            Json::Array(values) => Some(values),
            _ => None,
        }
    }

    pub fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Object(fields) => fields.iter().find(|(name, _)| name == key).map(|(_, v)| v),
            _ => None,
        }
    }

    fn type_name(&self) -> &'static str {
        match self {
            Json::Null => "null",
            Json::Bool(_) => "boolean",
            Json::Integer(_) => "integer",
            Json::Number(_) => "number",
            Json::String(_) => "string",
            Json::Array(_) => "array",
            Json::Object(_) => "object",
        }
    }
}

pub fn json_string(value: impl Into<String>) -> Json {
    Json::String(value.into())
}

pub fn json_num(value: i64) -> Json {
    Json::Integer(value)
}

pub fn json_array(values: Vec<Json>) -> Json {
    Json::Array(values)
}

pub fn json_object(fields: Vec<(&str, Json)>) -> Json {
    Json::Object(
        fields
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect(),
    )
}

/* ------------------------------- JSON parser ------------------------------ */

struct Parser {
    chars: Vec<char>,
    pos: usize,
}

impl Parser {
    fn parse(input: &str) -> Result<Json, String> {
        let mut parser = Parser {
            chars: input.chars().collect(),
            pos: 0,
        };
        parser.skip_ws();
        let value = parser.parse_value()?;
        parser.skip_ws();
        if parser.pos != parser.chars.len() {
            return Err(format!(
                "unexpected character {} after JSON value",
                parser.peek().unwrap_or(&'?')
            ));
        }
        Ok(value)
    }

    fn peek(&self) -> Option<&char> {
        self.chars.get(self.pos)
    }

    fn bump(&mut self) -> Option<char> {
        let value = self.chars.get(self.pos).copied();
        if value.is_some() {
            self.pos += 1;
        }
        value
    }

    fn eat(&mut self, expected: char) -> Result<(), String> {
        match self.bump() {
            Some(actual) if actual == expected => Ok(()),
            Some(actual) => Err(format!("expected '{}', found '{}'", expected, actual)),
            None => Err(format!("expected '{}', found end of input", expected)),
        }
    }

    fn skip_ws(&mut self) {
        while matches!(self.peek(), Some(c) if c.is_whitespace()) {
            self.pos += 1;
        }
    }

    fn parse_value(&mut self) -> Result<Json, String> {
        match self.peek() {
            Some('"') => {
                self.bump();
                Ok(Json::String(self.parse_string()?))
            }
            Some('[') => self.parse_array(),
            Some('{') => self.parse_object(),
            Some('t') | Some('f') => self.parse_bool(),
            Some('n') => self.parse_null(),
            Some(c) if *c == '-' || c.is_ascii_digit() => self.parse_number(),
            Some(c) => Err(format!("unexpected character '{}'", c)),
            None => Err("unexpected end of input".to_string()),
        }
    }

    fn parse_string(&mut self) -> Result<String, String> {
        let mut out = String::new();
        loop {
            match self.bump() {
                Some('"') => return Ok(out),
                Some('\\') => match self.bump() {
                    Some('"') => out.push('"'),
                    Some('\\') => out.push('\\'),
                    Some('/') => out.push('/'),
                    Some('b') => out.push('\u{0008}'),
                    Some('f') => out.push('\u{000C}'),
                    Some('n') => out.push('\n'),
                    Some('r') => out.push('\r'),
                    Some('t') => out.push('\t'),
                    Some('u') => {
                        let mut code = self.parse_hex4()?;
                        if (0xD800..=0xDBFF).contains(&code) {
                            match (self.bump(), self.bump()) {
                                (Some('\\'), Some('u')) => {
                                    let low = self.parse_hex4()?;
                                    if !(0xDC00..=0xDFFF).contains(&low) {
                                        return Err("invalid UTF-16 low surrogate".to_string());
                                    }
                                    code = 0x10000 + ((code - 0xD800) << 10) + (low - 0xDC00);
                                }
                                _ => {
                                    return Err("expected escaped UTF-16 low surrogate".to_string())
                                }
                            }
                        } else if (0xDC00..=0xDFFF).contains(&code) {
                            return Err("unexpected UTF-16 low surrogate".to_string());
                        }
                        out.push(
                            char::from_u32(code)
                                .ok_or_else(|| "invalid Unicode escape".to_string())?,
                        );
                    }
                    Some(other) => return Err(format!("invalid string escape '\\{}'", other)),
                    None => return Err("unterminated string escape".to_string()),
                },
                Some(c) if (c as u32) < 0x20 => {
                    return Err("unescaped control character in string".to_string())
                }
                Some(c) => out.push(c),
                None => return Err("unterminated string".to_string()),
            }
        }
    }

    fn parse_hex4(&mut self) -> Result<u32, String> {
        let mut value = 0u32;
        for _ in 0..4 {
            let digit = self
                .bump()
                .ok_or_else(|| "incomplete Unicode escape".to_string())?;
            value = value * 16
                + digit
                    .to_digit(16)
                    .ok_or_else(|| format!("invalid hexadecimal digit '{}'", digit))?;
        }
        Ok(value)
    }

    fn parse_array(&mut self) -> Result<Json, String> {
        self.eat('[')?;
        let mut values = Vec::new();
        self.skip_ws();
        if matches!(self.peek(), Some(']')) {
            self.bump();
            return Ok(Json::Array(values));
        }
        loop {
            values.push(self.parse_value()?);
            self.skip_ws();
            match self.bump() {
                Some(',') => {
                    self.skip_ws();
                }
                Some(']') => return Ok(Json::Array(values)),
                Some(other) => return Err(format!("expected ',' or ']', found '{}'", other)),
                None => return Err("unterminated array".to_string()),
            }
        }
    }

    fn parse_object(&mut self) -> Result<Json, String> {
        self.eat('{')?;
        let mut fields = Vec::new();
        self.skip_ws();
        if matches!(self.peek(), Some('}')) {
            self.bump();
            return Ok(Json::Object(fields));
        }
        loop {
            self.skip_ws();
            match self.bump() {
                Some('"') => {}
                Some(other) => {
                    return Err(format!("expected object key string, found '{}'", other))
                }
                None => return Err("unterminated object".to_string()),
            }
            let key = self.parse_string()?;
            self.skip_ws();
            self.eat(':')?;
            self.skip_ws();
            let value = self.parse_value()?;
            fields.push((key, value));
            self.skip_ws();
            match self.bump() {
                Some(',') => self.skip_ws(),
                Some('}') => return Ok(Json::Object(fields)),
                Some(other) => return Err(format!("expected ',' or '}}', found '{}'", other)),
                None => return Err("unterminated object".to_string()),
            }
        }
    }

    fn parse_bool(&mut self) -> Result<Json, String> {
        if self.starts_with_literal("true") {
            self.pos += 4;
            Ok(Json::Bool(true))
        } else if self.starts_with_literal("false") {
            self.pos += 5;
            Ok(Json::Bool(false))
        } else {
            Err("invalid literal".to_string())
        }
    }

    fn parse_null(&mut self) -> Result<Json, String> {
        if self.starts_with_literal("null") {
            self.pos += 4;
            Ok(Json::Null)
        } else {
            Err("invalid literal".to_string())
        }
    }

    fn starts_with_literal(&self, literal: &str) -> bool {
        let expected: Vec<char> = literal.chars().collect();
        self.chars
            .get(self.pos..self.pos + expected.len())
            .is_some_and(|actual| actual == expected)
    }

    fn parse_number(&mut self) -> Result<Json, String> {
        let start = self.pos;
        if matches!(self.peek(), Some('-')) {
            self.bump();
        }

        match self.peek() {
            Some('0') => {
                self.bump();
                if matches!(self.peek(), Some(c) if c.is_ascii_digit()) {
                    return Err("numbers may not contain leading zeroes".to_string());
                }
            }
            Some(c) if ('1'..='9').contains(c) => {
                while matches!(self.peek(), Some(c) if c.is_ascii_digit()) {
                    self.bump();
                }
            }
            _ => return Err("invalid number".to_string()),
        }

        if matches!(self.peek(), Some('.')) {
            self.bump();
            if !matches!(self.peek(), Some(c) if c.is_ascii_digit()) {
                return Err("fractional number requires a digit after '.'".to_string());
            }
            while matches!(self.peek(), Some(c) if c.is_ascii_digit()) {
                self.bump();
            }
        }

        if matches!(self.peek(), Some('e') | Some('E')) {
            self.bump();
            if matches!(self.peek(), Some('+') | Some('-')) {
                self.bump();
            }
            if !matches!(self.peek(), Some(c) if c.is_ascii_digit()) {
                return Err("number exponent requires a digit".to_string());
            }
            while matches!(self.peek(), Some(c) if c.is_ascii_digit()) {
                self.bump();
            }
        }

        let raw: String = self.chars[start..self.pos].iter().collect();
        if raw.contains(['.', 'e', 'E']) {
            raw.parse::<f64>()
                .map(Json::Number)
                .map_err(|_| format!("invalid number '{}'", raw))
        } else {
            raw.parse::<i64>()
                .map(Json::Integer)
                .map_err(|_| format!("invalid integer '{}'", raw))
        }
    }
}

/* ----------------------------- JSON rendering ----------------------------- */

pub fn to_pretty_json(value: &Json) -> String {
    let mut out = String::new();
    write_pretty(value, 0, &mut out);
    out.push('\n');
    out
}

fn write_pretty(value: &Json, indent: usize, out: &mut String) {
    match value {
        Json::Null => out.push_str("null"),
        Json::Bool(true) => out.push_str("true"),
        Json::Bool(false) => out.push_str("false"),
        Json::Integer(n) => out.push_str(&n.to_string()),
        Json::Number(n) => out.push_str(&format_json_number(*n)),
        Json::String(s) => write_json_string(s, out),
        Json::Array(values) => {
            if values.is_empty() {
                out.push_str("[]");
            } else {
                out.push_str("[\n");
                for (i, item) in values.iter().enumerate() {
                    push_indent(indent + 1, out);
                    write_pretty(item, indent + 1, out);
                    if i + 1 < values.len() {
                        out.push(',');
                    }
                    out.push('\n');
                }
                push_indent(indent, out);
                out.push(']');
            }
        }
        Json::Object(fields) => {
            if fields.is_empty() {
                out.push_str("{}");
            } else {
                out.push_str("{\n");
                for (i, (key, val)) in fields.iter().enumerate() {
                    push_indent(indent + 1, out);
                    write_json_string(key, out);
                    out.push_str(": ");
                    write_pretty(val, indent + 1, out);
                    if i + 1 < fields.len() {
                        out.push(',');
                    }
                    out.push('\n');
                }
                push_indent(indent, out);
                out.push('}');
            }
        }
    }
}

fn push_indent(indent: usize, out: &mut String) {
    for _ in 0..indent {
        out.push_str("  ");
    }
}

fn format_json_number(value: f64) -> String {
    if value.fract() == 0.0 && value.is_finite() {
        format!("{}", value as i64)
    } else {
        format!("{}", value)
    }
}

fn write_json_string(value: &str, out: &mut String) {
    out.push('"');
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{0008}' => out.push_str("\\b"),
            '\u{000C}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

pub fn parse_json(input: &str) -> Result<Json, String> {
    Parser::parse(input)
}

/* ------------------------------- Phylogeny ------------------------------- */

/// Reduced weighted support rate: weighted support / total tree weight.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rate {
    pub numerator: u32,
    pub denominator: u32,
}

impl Rate {
    fn reduce(numerator: u32, denominator: u32) -> Rate {
        let divisor = gcd(numerator, denominator);
        Rate {
            numerator: numerator / divisor,
            denominator: denominator / divisor,
        }
    }

    fn fraction(&self) -> String {
        format!("{}/{}", self.numerator, self.denominator)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConflictWitness {
    pub leaves: Vec<String>,
    pub weighted_support: u32,
    pub support: Rate,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupReport {
    pub leaves: Vec<String>,
    pub weighted_support: u32,
    pub support: Rate,
    pub accepted: bool,
    pub conflict_with: Option<ConflictWitness>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Analysis {
    pub leaves: Vec<String>,
    pub tree_count: usize,
    pub total_weight: u32,
    pub groups: Vec<GroupReport>,
    pub selected: Vec<u32>,
}

fn gcd(mut a: u32, mut b: u32) -> u32 {
    while b != 0 {
        let remainder = a % b;
        a = b;
        b = remainder;
    }
    a
}

fn require_root_object(value: &Json) -> Result<(), String> {
    match value {
        Json::Object(_) => Ok(()),
        other => Err(format!(
            "input must be a JSON object, got {}",
            other.type_name()
        )),
    }
}

fn require_array<'a>(value: &'a Json, field: &str) -> Result<&'a Vec<Json>, String> {
    match value.get(field) {
        Some(Json::Array(items)) => Ok(items),
        Some(other) => Err(format!(
            "field '{}' must be an array, got {}",
            field,
            other.type_name()
        )),
        None => Err(format!("missing required array field '{}'", field)),
    }
}

fn integer_at(value: &Json, field: &str) -> Result<i64, String> {
    match value {
        Json::Integer(value) => Ok(*value),
        Json::Number(_) => Err(format!(
            "field '{}' must be an integer, got a non-integer number",
            field
        )),
        other => Err(format!(
            "field '{}' must be an integer, got {}",
            field,
            other.type_name()
        )),
    }
}

fn parse_tree(
    node: &Json,
    is_root: bool,
    names: &[String],
    seen: &mut HashSet<u32>,
    clades: &mut Vec<u32>,
    tree_index: usize,
) -> Result<u32, String> {
    if let Some(name) = node.as_str() {
        let index = names
            .iter()
            .position(|candidate| candidate == name)
            .ok_or_else(|| format!("tree {} contains unknown leaf '{}'", tree_index + 1, name))?;
        let bit = 1u32 << index;
        if !seen.insert(bit) {
            return Err(format!(
                "tree {} contains leaf '{}' more than once",
                tree_index + 1,
                name
            ));
        }
        return Ok(bit);
    }

    let children = node.as_array().ok_or_else(|| {
        format!(
            "tree {} contains a node that is neither a leaf string nor an array",
            tree_index + 1
        )
    })?;
    if children.len() != 2 {
        return Err(format!(
            "tree {} has an internal node with {} children; rooted binary trees require exactly two",
            tree_index + 1,
            children.len()
        ));
    }

    let left = parse_tree(&children[0], false, names, seen, clades, tree_index)?;
    let right = parse_tree(&children[1], false, names, seen, clades, tree_index)?;
    if left & right != 0 {
        return Err(format!("tree {} contains duplicate leaves", tree_index + 1));
    }
    let mask = left | right;
    if !is_root {
        clades.push(mask);
    }
    Ok(mask)
}

/// Parse and analyze a complete request document.
pub fn analyze(input: &Json) -> Result<Analysis, String> {
    require_root_object(input)?;

    let leaf_values = require_array(input, "leaves")?;
    if !(3..=20).contains(&leaf_values.len()) {
        return Err(format!(
            "'leaves' must contain between 3 and 20 names, found {}",
            leaf_values.len()
        ));
    }

    let mut leaves = Vec::with_capacity(leaf_values.len());
    for value in leaf_values {
        let name = value
            .as_str()
            .ok_or_else(|| format!("each leaf must be a string, got {}", value.type_name()))?;
        if name.is_empty() || !name.chars().all(|c| (' '..='~').contains(&c)) {
            return Err(format!(
                "leaf '{}' must be non-empty printable ASCII text",
                name
            ));
        }
        leaves.push(name.to_string());
    }
    leaves.sort();
    let mut unique = HashSet::new();
    for leaf in &leaves {
        if !unique.insert(leaf.clone()) {
            return Err(format!("duplicate leaf '{}'", leaf));
        }
    }
    let n = leaves.len();
    let universe = if n == 32 { u32::MAX } else { (1u32 << n) - 1 };

    let tree_values = require_array(input, "trees")?;
    if !(2..=30).contains(&tree_values.len()) {
        return Err(format!(
            "'trees' must contain between 2 and 30 trees, found {}",
            tree_values.len()
        ));
    }

    let weight_values = require_array(input, "weights")?;
    if weight_values.len() != tree_values.len() {
        return Err(format!(
            "'weights' must contain one integer per tree ({} weights for {} trees)",
            weight_values.len(),
            tree_values.len()
        ));
    }
    let mut weights = Vec::with_capacity(weight_values.len());
    for value in weight_values {
        let weight = integer_at(value, "weights")?;
        if !(1..=10).contains(&weight) {
            return Err(format!(
                "each weight must be between 1 and 10, found {}",
                weight
            ));
        }
        weights.push(weight as u32);
    }

    let mut support = vec![0u32; 1usize << n];
    for (tree_index, tree) in tree_values.iter().enumerate() {
        let mut seen = HashSet::new();
        let mut clades = Vec::new();
        let root_mask = parse_tree(tree, true, &leaves, &mut seen, &mut clades, tree_index)?;
        if root_mask != universe {
            let missing: Vec<&String> = leaves
                .iter()
                .enumerate()
                .filter(|(i, _)| root_mask & (1u32 << *i) == 0)
                .map(|(_, name)| name)
                .collect();
            return Err(format!(
                "tree {} must contain every leaf exactly once; missing {}",
                tree_index + 1,
                missing
                    .iter()
                    .map(|name| format!("'{}'", name))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        for clade in clades {
            support[clade as usize] += weights[tree_index];
        }
    }

    let total_weight: u32 = weights.iter().sum();

    // Only strict, non-singleton clades are candidates.  The full leaf set is
    // implicit in every rooted tree and is represented directly by the root.
    let mut candidates: Vec<u32> = (1..universe)
        .filter(|mask| mask.count_ones() >= 2)
        .filter(|mask| support[*mask as usize] > 0)
        .collect();

    candidates.sort_by(|&a, &b| {
        support[b as usize]
            .cmp(&support[a as usize])
            .then_with(|| a.count_ones().cmp(&b.count_ones()))
            .then_with(|| leaf_mask_cmp(a, b, n))
    });

    let mut selected: Vec<u32> = Vec::new();
    let mut groups = Vec::with_capacity(candidates.len());

    for mask in candidates {
        let weighted_support = support[mask as usize];
        let leaves_for_mask = mask_to_leaves(mask, &leaves);
        let first_conflict = selected
            .iter()
            .copied()
            .find(|accepted| clades_conflict(mask, *accepted));

        let accepted = first_conflict.is_none();
        if accepted {
            selected.push(mask);
        }

        let conflict_with = first_conflict.map(|conflict_mask| ConflictWitness {
            leaves: mask_to_leaves(conflict_mask, &leaves),
            weighted_support: support[conflict_mask as usize],
            support: Rate::reduce(support[conflict_mask as usize], total_weight),
        });

        groups.push(GroupReport {
            leaves: leaves_for_mask,
            weighted_support,
            support: Rate::reduce(weighted_support, total_weight),
            accepted,
            conflict_with,
        });
    }

    Ok(Analysis {
        leaves,
        tree_count: tree_values.len(),
        total_weight,
        groups,
        selected,
    })
}

/// Compare two equal-size masks according to their sorted leaf-name vectors.
/// At the first leaf where they differ, the set containing that lexicographically
/// earlier leaf is smaller.
fn leaf_mask_cmp(a: u32, b: u32, bit_count: usize) -> std::cmp::Ordering {
    for i in 0..bit_count {
        let in_a = (a >> i) & 1;
        let in_b = (b >> i) & 1;
        if in_a != in_b {
            return if in_a == 1 {
                std::cmp::Ordering::Less
            } else {
                std::cmp::Ordering::Greater
            };
        }
    }
    std::cmp::Ordering::Equal
}

fn mask_to_leaves(mask: u32, leaves: &[String]) -> Vec<String> {
    leaves
        .iter()
        .enumerate()
        .filter(|(i, _)| mask & (1u32 << *i) != 0)
        .map(|(_, name)| name.clone())
        .collect()
}

/// Two sets are incompatible when they overlap but neither contains the other.
fn clades_conflict(a: u32, b: u32) -> bool {
    let intersection = a & b;
    intersection != 0 && intersection != a && intersection != b
}

/* --------------------------- Consensus rendering -------------------------- */

pub fn build_consensus(analysis: &Analysis) -> Json {
    let n = analysis.leaves.len();
    let root = (1u32 << n) - 1;
    let mut support = vec![0u32; 1usize << n];
    for group in &analysis.groups {
        let mask = leaves_to_mask(&group.leaves, &analysis.leaves);
        support[mask as usize] = group.weighted_support;
    }
    build_node(
        root,
        &analysis.selected,
        &support,
        &analysis.leaves,
        analysis.total_weight,
    )
}

fn leaves_to_mask(group: &[String], leaves: &[String]) -> u32 {
    group
        .iter()
        .filter_map(|name| leaves.iter().position(|leaf| leaf == name))
        .fold(0u32, |mask, index| mask | (1u32 << index))
}

fn build_node(
    mask: u32,
    selected: &[u32],
    support: &[u32],
    leaves: &[String],
    total_weight: u32,
) -> Json {
    if mask.count_ones() == 1 {
        let index = mask.trailing_zeros() as usize;
        return json_object(vec![
            ("type", json_string("leaf")),
            ("name", json_string(leaves[index].clone())),
        ]);
    }

    let mut child_masks: Vec<u32> = selected
        .iter()
        .copied()
        .filter(|candidate| *candidate != mask && (*candidate & mask) == *candidate)
        .filter(|candidate| {
            !selected.iter().any(|middle| {
                *middle != mask
                    && *middle != *candidate
                    && (*middle & mask) == *middle
                    && candidate & *middle == *candidate
            })
        })
        .collect();
    child_masks.sort_by_key(|child| child.trailing_zeros());

    let mut covered = 0u32;
    let mut children: Vec<(u32, Json)> = Vec::new();
    for child_mask in child_masks {
        covered |= child_mask;
        children.push((
            child_mask.trailing_zeros(),
            build_node(child_mask, selected, support, leaves, total_weight),
        ));
    }

    let mut remaining = mask & !covered;
    while remaining != 0 {
        let bit = remaining.isolate_lowest_one();
        let index = bit.trailing_zeros();
        children.push((
            index,
            json_object(vec![
                ("type", json_string("leaf")),
                ("name", json_string(leaves[index as usize].clone())),
            ]),
        ));
        remaining &= remaining - 1;
    }
    children.sort_by_key(|(minimum, _)| *minimum);

    let is_root = mask == (1u32 << leaves.len()) - 1;
    let weighted_support = if is_root {
        total_weight
    } else {
        support[mask as usize]
    };

    json_object(vec![
        ("type", json_string("clade")),
        (
            "leaves",
            json_array(
                mask_to_leaves(mask, leaves)
                    .into_iter()
                    .map(json_string)
                    .collect(),
            ),
        ),
        ("size", json_num(mask.count_ones() as i64)),
        ("weighted_support", json_num(weighted_support as i64)),
        (
            "support",
            rate_json(&Rate::reduce(weighted_support, total_weight)),
        ),
        (
            "children",
            json_array(children.into_iter().map(|(_, node)| node).collect()),
        ),
    ])
}

fn rate_json(rate: &Rate) -> Json {
    json_object(vec![
        ("numerator", json_num(rate.numerator as i64)),
        ("denominator", json_num(rate.denominator as i64)),
        ("fraction", json_string(rate.fraction())),
    ])
}

fn witness_json(witness: &ConflictWitness) -> Json {
    json_object(vec![
        (
            "leaves",
            json_array(witness.leaves.iter().cloned().map(json_string).collect()),
        ),
        ("size", json_num(witness.leaves.len() as i64)),
        (
            "weighted_support",
            json_num(witness.weighted_support as i64),
        ),
        ("support", rate_json(&witness.support)),
    ])
}

fn group_json(group: &GroupReport) -> Json {
    let conflict = group
        .conflict_with
        .as_ref()
        .map(witness_json)
        .unwrap_or(Json::Null);
    json_object(vec![
        (
            "leaves",
            json_array(group.leaves.iter().cloned().map(json_string).collect()),
        ),
        ("size", json_num(group.leaves.len() as i64)),
        ("weighted_support", json_num(group.weighted_support as i64)),
        ("support", rate_json(&group.support)),
        (
            "status",
            json_string(if group.accepted {
                "accepted"
            } else {
                "rejected"
            }),
        ),
        ("accepted", Json::Bool(group.accepted)),
        ("first_conflict_with", conflict),
    ])
}

/// Render an analysis as the documented response document.
pub fn result_json(analysis: &Analysis) -> Json {
    json_object(vec![
        (
            "leaves",
            json_array(analysis.leaves.iter().cloned().map(json_string).collect()),
        ),
        ("tree_count", json_num(analysis.tree_count as i64)),
        ("total_weight", json_num(analysis.total_weight as i64)),
        (
            "groups",
            json_array(analysis.groups.iter().map(group_json).collect()),
        ),
        (
            "selected_groups",
            json_array(
                analysis
                    .groups
                    .iter()
                    .filter(|group| group.accepted)
                    .map(|group| {
                        json_array(group.leaves.iter().cloned().map(json_string).collect())
                    })
                    .collect(),
            ),
        ),
        ("consensus", build_consensus(analysis)),
    ])
}

/* --------------------------------- Binary --------------------------------- */

fn run() -> Result<(), String> {
    let args: Vec<String> = std::env::args().collect();
    let mut input_text = String::new();
    match args.len() {
        1 => {
            std::io::stdin()
                .read_to_string(&mut input_text)
                .map_err(|error| format!("failed to read standard input: {}", error))?;
        }
        2 => {
            input_text = fs::read_to_string(&args[1])
                .map_err(|error| format!("failed to read '{}': {}", args[1], error))?;
        }
        _ => return Err("usage: clades [input.json]".to_string()),
    }

    let input = parse_json(&input_text)?;
    let analysis = analyze(&input)?;
    let output = result_json(&analysis);
    print!("{}", to_pretty_json(&output));
    Ok(())
}

fn main() {
    if let Err(error) = run() {
        eprintln!("clades: {}", error);
        process::exit(1);
    }
}

/* --------------------------------- Tests ---------------------------------- */

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(input: &str) -> Json {
        parse_json(input).expect("valid test JSON")
    }

    fn analyze_str(input: &str) -> Analysis {
        analyze(&parse(input)).expect("valid analysis input")
    }

    fn find_group<'a>(analysis: &'a Analysis, wanted: &[&str]) -> &'a GroupReport {
        analysis
            .groups
            .iter()
            .find(|group| group.leaves.iter().map(String::as_str).collect::<Vec<_>>() == wanted)
            .unwrap_or_else(|| panic!("missing group {:?}", wanted))
    }

    fn tree_clades_independent(node: &Json, leaves: &HashSet<String>) -> Vec<HashSet<String>> {
        fn enumerate(
            node: &Json,
            leaves: &HashSet<String>,
        ) -> (HashSet<String>, Vec<HashSet<String>>) {
            if let Some(name) = node.as_str() {
                assert!(leaves.contains(name));
                return (HashSet::from([name.to_string()]), Vec::new());
            }
            let children = node.as_array().expect("binary node array");
            assert_eq!(children.len(), 2);
            let (left_root, mut left_clades) = enumerate(&children[0], leaves);
            let (right_root, right_clades) = enumerate(&children[1], leaves);
            assert!(left_root.is_disjoint(&right_root));
            let root: HashSet<String> = left_root.union(&right_root).cloned().collect();
            left_clades.extend(right_clades);
            left_clades.push(root.clone());
            (root, left_clades)
        }

        enumerate(node, leaves).1
    }

    fn weighted_support_by_independent_enumeration(input: &Json) -> Vec<(HashSet<String>, u32)> {
        let leaf_names: HashSet<String> = input
            .get("leaves")
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();
        let trees = input.get("trees").unwrap().as_array().unwrap();
        let weights: Vec<u32> = input
            .get("weights")
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .map(|v| integer_at(v, "weights").unwrap() as u32)
            .collect();
        let mut scores: Vec<(HashSet<String>, u32)> = Vec::new();
        for (tree, weight) in trees.iter().zip(weights) {
            for clade in tree_clades_independent(tree, &leaf_names) {
                if clade.len() >= 2 && clade.len() < leaf_names.len() {
                    if let Some(entry) = scores.iter_mut().find(|(set, _)| set == &clade) {
                        entry.1 += weight;
                    } else {
                        scores.push((clade, weight));
                    }
                }
            }
        }
        scores
    }

    #[test]
    fn independently_enumerates_small_tree_support() {
        let input = r#"{
            "leaves": ["A", "B", "C", "D"],
            "trees": [
                [[["A", "B"], "C"], "D"],
                [["A", "B"], ["C", "D"]],
                [["A", ["B", "C"]], "D"]
            ],
            "weights": [2, 3, 4]
        }"#;
        let document = parse(input);
        let analysis = analyze(&document).unwrap();
        let independent = weighted_support_by_independent_enumeration(&document);

        for (set, expected_support) in independent {
            let mut leaves: Vec<String> = set.into_iter().collect();
            leaves.sort();
            let group = find_group(
                &analysis,
                &leaves.iter().map(String::as_str).collect::<Vec<_>>(),
            );
            assert_eq!(group.weighted_support, expected_support);
            assert_eq!(
                group.support,
                Rate::reduce(expected_support, analysis.total_weight)
            );
        }

        assert_eq!(find_group(&analysis, &["A", "B"]).weighted_support, 5);
        assert_eq!(find_group(&analysis, &["B", "C"]).weighted_support, 4);
        assert_eq!(find_group(&analysis, &["C", "D"]).weighted_support, 3);
        assert_eq!(find_group(&analysis, &["A", "B", "C"]).weighted_support, 6);
        assert_eq!(analysis.groups.len(), 4);
    }

    #[test]
    fn greedy_order_and_first_conflict_witness_are_correct() {
        let input = r#"{
            "leaves": ["A", "B", "C", "D"],
            "trees": [
                [[["A", "B"], "C"], "D"],
                [["A", "B"], ["C", "D"]],
                [["A", ["B", "C"]], "D"]
            ],
            "weights": [2, 3, 4]
        }"#;
        let analysis = analyze_str(input);

        let order: Vec<Vec<String>> = analysis.groups.iter().map(|g| g.leaves.clone()).collect();
        assert_eq!(
            order,
            vec![
                vec![String::from("A"), String::from("B"), String::from("C")],
                vec![String::from("A"), String::from("B")],
                vec![String::from("B"), String::from("C")],
                vec![String::from("C"), String::from("D")],
            ]
        );

        let abc = find_group(&analysis, &["A", "B", "C"]);
        assert!(abc.accepted);
        assert!(abc.conflict_with.is_none());

        let ab = find_group(&analysis, &["A", "B"]);
        assert!(ab.accepted);
        assert!(ab.conflict_with.is_none());

        let bc = find_group(&analysis, &["B", "C"]);
        assert!(!bc.accepted);
        let witness = bc.conflict_with.as_ref().expect("conflict witness");
        assert_eq!(witness.leaves, vec!["A", "B"]);
        assert_eq!(witness.weighted_support, 5);

        let cd = find_group(&analysis, &["C", "D"]);
        assert!(!cd.accepted);
        let witness = cd.conflict_with.as_ref().expect("conflict witness");
        // ABC was admitted before AB and is the first selected incompatible set.
        assert_eq!(witness.leaves, vec!["A", "B", "C"]);
        assert_eq!(witness.weighted_support, 6);
    }

    #[test]
    fn result_is_invariant_to_tree_and_weight_permutation() {
        let original = r#"{
            "leaves": ["D", "B", "A", "C"],
            "trees": [
                [[["A", "B"], "C"], "D"],
                [["A", "B"], ["C", "D"]],
                [["A", ["B", "C"]], "D"]
            ],
            "weights": [2, 3, 4]
        }"#;
        let permuted = r#"{
            "leaves": ["A", "B", "C", "D"],
            "trees": [
                [["A", ["B", "C"]], "D"],
                [[["A", "B"], "C"], "D"],
                [["A", "B"], ["C", "D"]]
            ],
            "weights": [4, 2, 3]
        }"#;
        assert_eq!(analyze_str(original), analyze_str(permuted));
        let original_result = result_json(&analyze_str(original));
        let permuted_result = result_json(&analyze_str(permuted));
        assert_eq!(
            to_pretty_json(&original_result),
            to_pretty_json(&permuted_result)
        );
    }

    #[test]
    fn unanimous_trees_give_full_support_and_no_rejection() {
        let input = r#"{
            "leaves": ["A", "B", "C", "D"],
            "trees": [
                [[["A", "B"], "C"], "D"],
                [[["A", "B"], "C"], "D"],
                [[["A", "B"], "C"], "D"]
            ],
            "weights": [1, 7, 2]
        }"#;
        let analysis = analyze_str(input);
        assert_eq!(analysis.total_weight, 10);
        for group in &analysis.groups {
            assert!(group.accepted);
            assert_eq!(
                group.support,
                Rate {
                    numerator: 1,
                    denominator: 1
                }
            );
            assert_eq!(group.weighted_support, 10);
            assert!(group.conflict_with.is_none());
        }
        assert_eq!(analysis.selected, vec![0b0011, 0b0111]);

        let consensus = build_consensus(&analysis);
        let expected = parse(
            r#"{
                "type": "clade",
                "leaves": ["A", "B", "C", "D"],
                "size": 4,
                "weighted_support": 10,
                "support": {"numerator": 1, "denominator": 1, "fraction": "1/1"},
                "children": [
                    {
                        "type": "clade",
                        "leaves": ["A", "B", "C"],
                        "size": 3,
                        "weighted_support": 10,
                        "support": {"numerator": 1, "denominator": 1, "fraction": "1/1"},
                        "children": [
                            {
                                "type": "clade",
                                "leaves": ["A", "B"],
                                "size": 2,
                                "weighted_support": 10,
                                "support": {"numerator": 1, "denominator": 1, "fraction": "1/1"},
                                "children": [
                                    {"type": "leaf", "name": "A"},
                                    {"type": "leaf", "name": "B"}
                                ]
                            },
                            {"type": "leaf", "name": "C"}
                        ]
                    },
                    {"type": "leaf", "name": "D"}
                ]
            }"#,
        );
        assert_eq!(to_pretty_json(&consensus), to_pretty_json(&expected));
    }

    #[test]
    fn unresolved_compatible_groups_become_a_polytomy() {
        let input = r#"{
            "leaves": ["A", "B", "C"],
            "trees": [[["A", "B"], "C"], [["A", "C"], "B"]],
            "weights": [1, 1]
        }"#;
        let analysis = analyze_str(input);
        assert!(find_group(&analysis, &["A", "B"]).accepted);
        assert!(!find_group(&analysis, &["A", "C"]).accepted);
        let consensus = build_consensus(&analysis);
        let children = consensus.get("children").unwrap().as_array().unwrap();
        assert_eq!(children.len(), 2);
        assert_eq!(children[0].get("type").unwrap().as_str(), Some("clade"));
        assert_eq!(children[1].get("name").unwrap().as_str(), Some("C"));
    }

    #[test]
    fn rejects_invalid_trees_and_weights() {
        let valid_control =
            r#"{"leaves":["A","B","C"],"trees":[[["A","B"],"C"],[["A","C"],"B"]],"weights":[1,1]}"#;
        let _ = analyze_str(valid_control);

        let missing = r#"{
            "leaves":["A","B","C"],
            "trees":[["A","B"],[["A","C"],"B"]],
            "weights":[1,1]
        }"#;
        assert!(analyze(&parse(missing)).is_err());

        let duplicate = r#"{
            "leaves":["A","B","C"],
            "trees":[["A","A"],[["A","C"],"B"]],
            "weights":[1,1]
        }"#;
        assert!(analyze(&parse(duplicate)).is_err());

        let nonbinary = r#"{
            "leaves":["A","B","C"],
            "trees":[["A","B","C"],[["A","C"],"B"]],
            "weights":[1,1]
        }"#;
        assert!(analyze(&parse(nonbinary)).is_err());

        let bad_weight = r#"{
            "leaves":["A","B","C"],
            "trees":[[["A","B"],"C"],[["A","C"],"B"]],
            "weights":[1,11]
        }"#;
        assert!(analyze(&parse(bad_weight)).is_err());

        let fractional_weight = r#"{
            "leaves":["A","B","C"],
            "trees":[[["A","B"],"C"],[["A","C"],"B"]],
            "weights":[1,1.0]
        }"#;
        assert!(analyze(&parse(fractional_weight)).is_err());
    }
}
