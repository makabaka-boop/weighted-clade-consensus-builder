//! Weighted clade consensus builder.
//!
//! Different researchers rarely draw the same bifurcating tree for one set of
//! taxa, and naively stuffing every popular split into a single tree produces
//! impossible nestings. This crate reads a JSON document describing weighted
//! rooted binary trees over a shared leaf set, scores every non-trivial clade
//! by the summed weights of the trees containing it, greedily keeps a
//! compatible (laminar) family of clades, and rebuilds a possibly
//! multifurcating consensus tree.
//!
//! Pipeline:
//! 1. Extract every non-trivial leaf set (2..=n-1 leaves) from each input
//!    tree and accumulate its weighted support.
//! 2. Order candidate clades by support descending, then size ascending,
//!    then the sorted leaf-name list lexicographically.
//! 3. Walk the ranking, accepting each clade that is compatible with every
//!    already-accepted clade; for a rejected clade record the *first*
//!    conflicting accepted clade as the witness.
//! 4. Rebuild a (possibly multifurcating) consensus tree from the accepted
//!    family, sorting each node's children by their minimum leaf name, and
//!    report every clade's support as a reduced fraction of the total weight.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeSet, HashMap};
use std::fmt;

/// Result alias for fallible operations in this crate.
pub type Result<T> = std::result::Result<T, Error>;

/// Input validation or (de)serialization failure.
#[derive(Debug)]
pub struct Error(String);

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

impl From<serde_json::Error> for Error {
    fn from(source: serde_json::Error) -> Self {
        Error(format!("invalid JSON: {source}"))
    }
}

fn err<T>(message: impl Into<String>) -> Result<T> {
    Err(Error(message.into()))
}

/// Top-level input document.
#[derive(Debug, Deserialize)]
pub struct Input {
    /// 3..=20 unique non-empty ASCII leaf names.
    pub leaves: Vec<String>,
    /// 2..=30 weighted rooted binary trees over exactly those leaves.
    pub trees: Vec<WeightedTree>,
}

/// One rooted binary tree together with its voting weight.
#[derive(Debug, Deserialize)]
pub struct WeightedTree {
    /// Integer weight in 1..=10.
    pub weight: u64,
    /// Nested arrays: a leaf is a string, an internal node is `[child, child]`.
    pub tree: Value,
}

/// A scored clade and its fate in the greedy selection.
#[derive(Debug, Serialize)]
pub struct Group {
    /// Sorted leaf names of the clade.
    pub leaves: Vec<String>,
    /// Summed weights of the input trees containing this clade.
    pub support: u64,
    /// `support / total_weight` as a reduced fraction, e.g. `"3/4"`.
    pub fraction: String,
    /// Whether the clade survived the greedy laminar selection.
    pub accepted: bool,
    /// For a rejected clade: the first accepted clade it conflicts with.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub conflict_with: Option<Vec<String>>,
}

/// The JSON report emitted on stdout.
#[derive(Debug, Serialize)]
pub struct Output {
    /// Sorted leaf names shared by all input trees.
    pub leaves: Vec<String>,
    /// Number of input trees.
    pub tree_count: usize,
    /// Sum of all tree weights.
    pub total_weight: u64,
    /// Every candidate clade, in processing order.
    pub groups: Vec<Group>,
    /// Consensus tree as nested arrays (strings are leaves).
    pub consensus_tree: Value,
}

/// Parsed rooted binary tree.
#[derive(Debug)]
enum Node {
    Leaf(String),
    Internal(Vec<Node>),
}

/// Parse a nested-array tree, checking that every leaf name is known.
fn parse_tree(value: &Value, known: &BTreeSet<String>) -> Result<Node> {
    match value {
        Value::String(name) => {
            if !known.contains(name) {
                return err(format!("tree references unknown leaf {name:?}"));
            }
            Ok(Node::Leaf(name.clone()))
        }
        Value::Array(children) => {
            if children.len() != 2 {
                return err(format!(
                    "internal nodes must be arrays of exactly 2 children, got {}",
                    children.len()
                ));
            }
            let mut parsed = Vec::with_capacity(2);
            for child in children {
                parsed.push(parse_tree(child, known)?);
            }
            Ok(Node::Internal(parsed))
        }
        other => err(format!(
            "tree nodes must be leaf-name strings or 2-element arrays, got {other}"
        )),
    }
}

/// Count leaf occurrences under `node`.
fn count_leaves(node: &Node, counts: &mut HashMap<String, usize>) {
    match node {
        Node::Leaf(name) => *counts.entry(name.clone()).or_insert(0) += 1,
        Node::Internal(children) => {
            for child in children {
                count_leaves(child, counts);
            }
        }
    }
}

/// Validate the input document and return the parsed trees with weights.
fn validate(input: &Input) -> Result<Vec<(u64, Node)>> {
    let leaf_count = input.leaves.len();
    if !(3..=20).contains(&leaf_count) {
        return err(format!("expected 3..=20 leaves, got {leaf_count}"));
    }
    let mut known = BTreeSet::new();
    for leaf in &input.leaves {
        if leaf.is_empty() || !leaf.is_ascii() {
            return err(format!(
                "leaf names must be non-empty ASCII strings, got {leaf:?}"
            ));
        }
        if !known.insert(leaf.clone()) {
            return err(format!("duplicate leaf name {leaf:?}"));
        }
    }
    if !(2..=30).contains(&input.trees.len()) {
        return err(format!("expected 2..=30 trees, got {}", input.trees.len()));
    }
    let mut trees = Vec::with_capacity(input.trees.len());
    for (index, weighted) in input.trees.iter().enumerate() {
        if !(1..=10).contains(&weighted.weight) {
            return err(format!(
                "tree {index}: weight must be in 1..=10, got {}",
                weighted.weight
            ));
        }
        let root = parse_tree(&weighted.tree, &known)?;
        let mut counts = HashMap::new();
        count_leaves(&root, &mut counts);
        for leaf in &input.leaves {
            match counts.get(leaf) {
                Some(1) => {}
                Some(k) => return err(format!("tree {index}: leaf {leaf:?} appears {k} times")),
                None => return err(format!("tree {index}: missing leaf {leaf:?}")),
            }
        }
        trees.push((weighted.weight, root));
    }
    Ok(trees)
}

/// Return the sorted leaf set of `node`, recording every non-trivial clade
/// (2..=n-1 leaves) below it into `out`.
fn collect_clades(node: &Node, leaf_count: usize, out: &mut Vec<Vec<String>>) -> Vec<String> {
    match node {
        Node::Leaf(name) => vec![name.clone()],
        Node::Internal(children) => {
            let mut set = Vec::new();
            for child in children {
                set.extend(collect_clades(child, leaf_count, out));
            }
            set.sort_unstable();
            if set.len() >= 2 && set.len() < leaf_count {
                out.push(set.clone());
            }
            set
        }
    }
}

/// Two leaf sets are compatible when they are disjoint or one contains the
/// other; only compatible clades can coexist in a single rooted tree.
/// Both slices must be sorted.
pub fn compatible(a: &[String], b: &[String]) -> bool {
    let (mut i, mut j) = (0, 0);
    let (mut a_extra, mut b_extra, mut shared) = (false, false, false);
    while i < a.len() && j < b.len() {
        match a[i].cmp(&b[j]) {
            std::cmp::Ordering::Less => {
                a_extra = true;
                i += 1;
            }
            std::cmp::Ordering::Greater => {
                b_extra = true;
                j += 1;
            }
            std::cmp::Ordering::Equal => {
                shared = true;
                i += 1;
                j += 1;
            }
        }
    }
    a_extra |= i < a.len();
    b_extra |= j < b.len();
    !a_extra || !b_extra || !shared
}

/// `true` when every element of sorted slice `a` occurs in sorted slice `b`.
fn is_subset(a: &[String], b: &[String]) -> bool {
    a.iter().all(|x| b.binary_search(x).is_ok())
}

fn gcd(mut a: u64, mut b: u64) -> u64 {
    while b != 0 {
        let t = a % b;
        a = b;
        b = t;
    }
    a
}

/// Rebuild a (possibly multifurcating) tree from a laminar family of clades.
fn rebuild(accepted: &[Vec<String>], all_leaves: &[String]) -> Value {
    // The parent of each accepted clade is the smallest accepted clade
    // strictly containing it, or the full leaf set (the root).
    let mut children_of: HashMap<&[String], Vec<&[String]>> = HashMap::new();
    for set in accepted {
        let parent = accepted
            .iter()
            .filter(|other| other.len() > set.len() && is_subset(set, other))
            .min_by_key(|other| other.len())
            .map(Vec::as_slice)
            .unwrap_or(all_leaves);
        children_of.entry(parent).or_default().push(set);
    }
    emit(all_leaves, &children_of)
}

/// Emit `set` as nested arrays; children are ordered by minimum leaf name.
fn emit(set: &[String], children_of: &HashMap<&[String], Vec<&[String]>>) -> Value {
    let mut parts: Vec<(&str, Value)> = Vec::new();
    if let Some(children) = children_of.get(set) {
        for child in children {
            parts.push((child[0].as_str(), emit(child, children_of)));
        }
    }
    for leaf in set {
        let covered = children_of
            .get(set)
            .is_some_and(|children| children.iter().any(|c| c.binary_search(leaf).is_ok()));
        if !covered {
            parts.push((leaf.as_str(), Value::String(leaf.clone())));
        }
    }
    parts.sort_by(|a, b| a.0.cmp(b.0));
    Value::Array(parts.into_iter().map(|(_, value)| value).collect())
}

/// Score every non-trivial clade, select a laminar family greedily, and
/// rebuild the consensus tree.
pub fn analyze(input: &Input) -> Result<Output> {
    let trees = validate(input)?;
    let leaf_count = input.leaves.len();
    let total_weight: u64 = trees.iter().map(|(weight, _)| weight).sum();

    // Weighted support of each non-trivial clade.
    let mut supports: HashMap<Vec<String>, u64> = HashMap::new();
    for (weight, root) in &trees {
        let mut clades = Vec::new();
        collect_clades(root, leaf_count, &mut clades);
        clades.sort_unstable();
        clades.dedup();
        for clade in clades {
            *supports.entry(clade).or_insert(0) += weight;
        }
    }

    // Rank: support descending, size ascending, leaf list lexicographic.
    let mut ranked: Vec<(Vec<String>, u64)> = supports.into_iter().collect();
    ranked.sort_by(|a, b| {
        b.1.cmp(&a.1)
            .then_with(|| a.0.len().cmp(&b.0.len()))
            .then_with(|| a.0.cmp(&b.0))
    });

    // Greedy laminar selection with conflict witnesses.
    let mut accepted: Vec<usize> = Vec::new();
    let mut groups = Vec::with_capacity(ranked.len());
    for (leaves, support) in &ranked {
        let witness = accepted
            .iter()
            .map(|&j| &ranked[j].0)
            .find(|chosen| !compatible(chosen, leaves));
        let divisor = gcd(*support, total_weight);
        groups.push(Group {
            leaves: leaves.clone(),
            support: *support,
            fraction: format!("{}/{}", support / divisor, total_weight / divisor),
            accepted: witness.is_none(),
            conflict_with: witness.cloned(),
        });
        if witness.is_none() {
            accepted.push(groups.len() - 1);
        }
    }

    let mut leaves = input.leaves.clone();
    leaves.sort_unstable();
    let accepted_sets: Vec<Vec<String>> = accepted.iter().map(|&j| ranked[j].0.clone()).collect();
    let consensus_tree = rebuild(&accepted_sets, &leaves);

    Ok(Output {
        leaves,
        tree_count: trees.len(),
        total_weight,
        groups,
        consensus_tree,
    })
}

/// Parse a JSON input document, analyze it, and return the pretty-printed
/// JSON report.
pub fn run(input_json: &str) -> Result<String> {
    let input: Input = serde_json::from_str(input_json)?;
    let output = analyze(&input)?;
    Ok(serde_json::to_string_pretty(&output)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(names: &[&str]) -> Vec<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn gcd_reduces_fractions() {
        assert_eq!(gcd(6, 8), 2);
        assert_eq!(gcd(5, 5), 5);
        assert_eq!(gcd(1, 10), 1);
        assert_eq!(gcd(7, 3), 1);
    }

    #[test]
    fn compatibility_means_disjoint_or_nested() {
        assert!(compatible(&set(&["a", "b"]), &set(&["c", "d"])));
        assert!(compatible(&set(&["a", "b"]), &set(&["a", "b", "c"])));
        assert!(compatible(&set(&["a", "b", "c"]), &set(&["b"])));
        assert!(!compatible(&set(&["a", "b"]), &set(&["b", "c"])));
        assert!(!compatible(&set(&["a", "c"]), &set(&["b", "c", "d"])));
    }
}
