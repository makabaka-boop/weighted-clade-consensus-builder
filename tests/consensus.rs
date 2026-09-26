//! Integration tests for the weighted clade consensus builder.
//!
//! The tests never call the library's clade extraction. Instead they
//! enumerate leaf subsets of small trees independently and decide clade
//! membership by locating the subset's lowest common ancestor in an arena
//! tree built from the raw JSON — a completely separate code path.

use serde_json::{json, Value};
use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io::Write;
use std::process::{Command, Stdio};

// ---------------------------------------------------------------------------
// Independent machinery: arena trees + LCA-based clade enumeration
// ---------------------------------------------------------------------------

struct Arena {
    leaf: Vec<Option<String>>,
    children: Vec<Vec<usize>>,
    parent: Vec<Option<usize>>,
}

fn build_arena(value: &Value) -> Arena {
    fn add(arena: &mut Arena, value: &Value, parent: Option<usize>) -> usize {
        let id = arena.leaf.len();
        arena.leaf.push(None);
        arena.children.push(Vec::new());
        arena.parent.push(parent);
        match value {
            Value::String(name) => arena.leaf[id] = Some(name.clone()),
            Value::Array(kids) => {
                for kid in kids {
                    let child = add(arena, kid, Some(id));
                    arena.children[id].push(child);
                }
            }
            other => panic!("bad test tree node: {other}"),
        }
        id
    }
    let mut arena = Arena {
        leaf: Vec::new(),
        children: Vec::new(),
        parent: Vec::new(),
    };
    add(&mut arena, value, None);
    arena
}

fn leaf_set(arena: &Arena, node: usize) -> BTreeSet<String> {
    let mut set = BTreeSet::new();
    let mut stack = vec![node];
    while let Some(n) = stack.pop() {
        if let Some(name) = &arena.leaf[n] {
            set.insert(name.clone());
        }
        stack.extend_from_slice(&arena.children[n]);
    }
    set
}

fn depth(arena: &Arena, mut node: usize) -> usize {
    let mut d = 0;
    while let Some(p) = arena.parent[node] {
        node = p;
        d += 1;
    }
    d
}

fn lca(arena: &Arena, a: usize, b: usize) -> usize {
    let (mut x, mut y) = (a, b);
    let (mut dx, mut dy) = (depth(arena, x), depth(arena, y));
    while dx > dy {
        x = arena.parent[x].unwrap();
        dx -= 1;
    }
    while dy > dx {
        y = arena.parent[y].unwrap();
        dy -= 1;
    }
    while x != y {
        x = arena.parent[x].unwrap();
        y = arena.parent[y].unwrap();
    }
    x
}

/// A leaf subset is a clade iff the LCA of its members spans exactly it.
fn is_clade(arena: &Arena, index: &HashMap<String, usize>, subset: &BTreeSet<String>) -> bool {
    let mut members = subset.iter().map(|name| index[name]);
    let mut node = members.next().unwrap();
    for member in members {
        node = lca(arena, node, member);
    }
    leaf_set(arena, node) == *subset
}

/// Independently compute weighted clade supports by enumerating all subsets.
fn brute_force_supports(leaves: &[String], trees: &[(u64, Value)]) -> BTreeMap<Vec<String>, u64> {
    let arenas: Vec<(u64, Arena, HashMap<String, usize>)> = trees
        .iter()
        .map(|(weight, tree)| {
            let arena = build_arena(tree);
            let index: HashMap<String, usize> = arena
                .leaf
                .iter()
                .enumerate()
                .filter_map(|(i, name)| name.clone().map(|n| (n, i)))
                .collect();
            (*weight, arena, index)
        })
        .collect();
    let n = leaves.len();
    let mut supports = BTreeMap::new();
    for mask in 0u32..(1u32 << n) {
        let size = mask.count_ones() as usize;
        if size < 2 || size >= n {
            continue;
        }
        let subset: BTreeSet<String> = (0..n)
            .filter(|i| mask & (1 << i) != 0)
            .map(|i| leaves[i].clone())
            .collect();
        let support: u64 = arenas
            .iter()
            .filter(|(_, arena, index)| is_clade(arena, index, &subset))
            .map(|(weight, _, _)| weight)
            .sum();
        if support > 0 {
            supports.insert(subset.into_iter().collect::<Vec<_>>(), support);
        }
    }
    supports
}

// ---------------------------------------------------------------------------
// Test helpers
// ---------------------------------------------------------------------------

fn tool_output(input: &Value) -> Value {
    let report = clades::run(&serde_json::to_string(input).unwrap()).unwrap();
    serde_json::from_str(&report).unwrap()
}

fn leaves_of(group: &Value) -> Vec<String> {
    group["leaves"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect()
}

fn compatible(a: &[String], b: &[String]) -> bool {
    let x: BTreeSet<&String> = a.iter().collect();
    let y: BTreeSet<&String> = b.iter().collect();
    x.is_disjoint(&y) || x.is_subset(&y) || y.is_subset(&x)
}

fn gcd(mut a: u64, mut b: u64) -> u64 {
    while b != 0 {
        let t = a % b;
        a = b;
        b = t;
    }
    a
}

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

fn shuffle(rng: &mut Rng, items: &mut [String]) {
    for i in (1..items.len()).rev() {
        let j = rng.below(i + 1);
        items.swap(i, j);
    }
}

fn random_tree(rng: &mut Rng, leaves: &[String]) -> Value {
    if leaves.len() == 1 {
        return json!(leaves[0]);
    }
    let k = 1 + rng.below(leaves.len() - 1);
    json!([
        random_tree(rng, &leaves[..k]),
        random_tree(rng, &leaves[k..])
    ])
}

/// A random valid input: shuffled leaf order, random binary trees, weights 1..=10.
fn random_input(rng: &mut Rng, leaf_count: usize, tree_count: usize) -> Value {
    let mut leaves: Vec<String> = (0..leaf_count).map(|i| format!("L{i}")).collect();
    shuffle(rng, &mut leaves);
    let trees: Vec<Value> = (0..tree_count)
        .map(|_| {
            let mut placement = leaves.clone();
            shuffle(rng, &mut placement);
            json!({
                "weight": 1 + rng.below(10) as u64,
                "tree": random_tree(rng, &placement),
            })
        })
        .collect();
    json!({ "leaves": leaves, "trees": trees })
}

fn weighted_trees(input: &Value) -> Vec<(u64, Value)> {
    input["trees"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| (t["weight"].as_u64().unwrap(), t["tree"].clone()))
        .collect()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
fn clade_supports_match_brute_force_enumeration() {
    let mut rng = Rng(0x5EED);
    for leaf_count in 3..=7 {
        for _ in 0..3 {
            let input = random_input(&mut rng, leaf_count, 6);
            let out = tool_output(&input);
            let leaves: Vec<String> = out["leaves"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_str().unwrap().to_string())
                .collect();
            let expected = brute_force_supports(&leaves, &weighted_trees(&input));
            let actual: BTreeMap<Vec<String>, u64> = out["groups"]
                .as_array()
                .unwrap()
                .iter()
                .map(|g| (leaves_of(g), g["support"].as_u64().unwrap()))
                .collect();
            assert_eq!(actual, expected, "leaf_count={leaf_count}");
        }
    }
}

#[test]
fn greedy_selection_invariants_hold_on_random_inputs() {
    let mut rng = Rng(0xC1A0);
    for leaf_count in 3..=8 {
        let input = random_input(&mut rng, leaf_count, 8);
        let out = tool_output(&input);
        let total_weight = out["total_weight"].as_u64().unwrap();
        let groups = out["groups"].as_array().unwrap();

        // Groups are ranked by support desc, size asc, leaf list lexicographic.
        let key = |g: &Value| {
            (
                Reverse(g["support"].as_u64().unwrap()),
                g["leaves"].as_array().unwrap().len(),
                leaves_of(g),
            )
        };
        for pair in groups.windows(2) {
            assert!(key(&pair[0]) <= key(&pair[1]), "ranking violated");
        }

        // Accepted family is laminar; every witness is the *first* accepted
        // clade conflicting with the rejected one.
        let mut accepted: Vec<Vec<String>> = Vec::new();
        for group in groups {
            let leaves = leaves_of(group);
            if group["accepted"].as_bool().unwrap() {
                for prev in &accepted {
                    assert!(compatible(prev, &leaves), "accepted family not laminar");
                }
                accepted.push(leaves);
            } else {
                let witness: Vec<String> = group["conflict_with"]
                    .as_array()
                    .expect("rejected group must name a witness")
                    .iter()
                    .map(|v| v.as_str().unwrap().to_string())
                    .collect();
                let pos = accepted
                    .iter()
                    .position(|a| *a == witness)
                    .expect("witness must be an accepted clade");
                assert!(!compatible(&witness, &leaves), "witness does not conflict");
                for earlier in &accepted[..pos] {
                    assert!(
                        compatible(earlier, &leaves),
                        "an earlier accepted clade already conflicts"
                    );
                }
            }
        }

        // Fractions are reduced support / total_weight.
        for group in groups {
            let support = group["support"].as_u64().unwrap();
            let fraction = group["fraction"].as_str().unwrap();
            let (p, q) = fraction.split_once('/').expect("fraction has p/q form");
            let (p, q): (u64, u64) = (p.parse().unwrap(), q.parse().unwrap());
            let d = gcd(support, total_weight);
            assert_eq!((p, q), (support / d, total_weight / d));
        }

        // The consensus tree contains every leaf exactly once, its non-trivial
        // clades are exactly the accepted family, and each node's children are
        // ordered by minimum leaf name.
        let consensus = &out["consensus_tree"];
        let arena = build_arena(consensus);
        let mut counts: BTreeMap<String, usize> = BTreeMap::new();
        for name in arena.leaf.iter().flatten() {
            *counts.entry(name.clone()).or_insert(0) += 1;
        }
        let leaf_count = counts.len();
        let expected_counts: BTreeMap<String, usize> = out["leaves"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| (v.as_str().unwrap().to_string(), 1))
            .collect();
        assert_eq!(counts, expected_counts);

        let mut consensus_clades = BTreeSet::new();
        for node in 0..arena.leaf.len() {
            if arena.children[node].is_empty() {
                continue;
            }
            let set = leaf_set(&arena, node);
            if set.len() >= 2 && set.len() < leaf_count {
                consensus_clades.insert(set.into_iter().collect::<Vec<_>>());
            }
            let mins: Vec<String> = arena.children[node]
                .iter()
                .map(|&c| leaf_set(&arena, c).into_iter().next().unwrap())
                .collect();
            let mut sorted = mins.clone();
            sorted.sort();
            assert_eq!(mins, sorted, "children not ordered by minimum leaf");
        }
        let accepted_sets: BTreeSet<Vec<String>> = accepted.into_iter().collect();
        assert_eq!(consensus_clades, accepted_sets);
    }
}

#[test]
fn conflict_witnesses_are_recorded() {
    // {c,d} is compatible with the first accepted clade {a,b} but clashes
    // with the second one {d,e}: the witness must be {d,e}, proving the
    // "first conflicting accepted clade" semantics.
    let input = json!({
        "leaves": ["a", "b", "c", "d", "e"],
        "trees": [
            {"weight": 5, "tree": [["a", "b"], ["c", ["d", "e"]]]},
            {"weight": 3, "tree": [["c", "d"], ["a", ["b", "e"]]]}
        ]
    });
    let out = tool_output(&input);
    assert_eq!(out["tree_count"], json!(2));
    assert_eq!(out["total_weight"], json!(8));
    assert_eq!(out["leaves"], json!(["a", "b", "c", "d", "e"]));
    assert_eq!(
        out["groups"],
        json!([
            {"leaves": ["a", "b"], "support": 5, "fraction": "5/8", "accepted": true},
            {"leaves": ["d", "e"], "support": 5, "fraction": "5/8", "accepted": true},
            {"leaves": ["c", "d", "e"], "support": 5, "fraction": "5/8", "accepted": true},
            {"leaves": ["b", "e"], "support": 3, "fraction": "3/8", "accepted": false, "conflict_with": ["a", "b"]},
            {"leaves": ["c", "d"], "support": 3, "fraction": "3/8", "accepted": false, "conflict_with": ["d", "e"]},
            {"leaves": ["a", "b", "e"], "support": 3, "fraction": "3/8", "accepted": false, "conflict_with": ["d", "e"]}
        ])
    );
    assert_eq!(
        out["consensus_tree"],
        json!([["a", "b"], ["c", ["d", "e"]]])
    );
}

#[test]
fn fractions_are_reduced() {
    let input = json!({
        "leaves": ["a", "b", "c"],
        "trees": [
            {"weight": 3, "tree": ["a", ["b", "c"]]},
            {"weight": 1, "tree": ["b", ["a", "c"]]}
        ]
    });
    let out = tool_output(&input);
    assert_eq!(out["total_weight"], json!(4));
    assert_eq!(
        out["groups"],
        json!([
            {"leaves": ["b", "c"], "support": 3, "fraction": "3/4", "accepted": true},
            {"leaves": ["a", "c"], "support": 1, "fraction": "1/4", "accepted": false, "conflict_with": ["b", "c"]}
        ])
    );
}

#[test]
fn consensus_allows_multifurcation() {
    // No accepted clade contains `e`, so the root gets three children.
    let input = json!({
        "leaves": ["a", "b", "c", "d", "e"],
        "trees": [
            {"weight": 1, "tree": [[["a", "b"], "c"], ["d", "e"]]},
            {"weight": 1, "tree": [[["c", "d"], "a"], ["b", "e"]]}
        ]
    });
    let out = tool_output(&input);
    assert_eq!(out["consensus_tree"], json!([["a", "b"], ["c", "d"], "e"]));
}

#[test]
fn output_is_invariant_under_input_permutations() {
    let mut rng = Rng(0xBEEF);
    let input = random_input(&mut rng, 7, 8);
    let baseline = clades::run(&serde_json::to_string(&input).unwrap()).unwrap();

    let trees = input["trees"].as_array().unwrap();

    // Reverse, rotate, and shuffle the tree list.
    let mut reversed = input.clone();
    reversed["trees"] = Value::Array(trees.iter().rev().cloned().collect());
    assert_eq!(
        clades::run(&serde_json::to_string(&reversed).unwrap()).unwrap(),
        baseline
    );

    let mut rotated_vec = trees.clone();
    rotated_vec.rotate_left(3);
    let mut rotated = input.clone();
    rotated["trees"] = Value::Array(rotated_vec);
    assert_eq!(
        clades::run(&serde_json::to_string(&rotated).unwrap()).unwrap(),
        baseline
    );

    let mut shuffled_vec = trees.clone();
    let mut shuffler = Rng(42);
    for i in (1..shuffled_vec.len()).rev() {
        let j = shuffler.below(i + 1);
        shuffled_vec.swap(i, j);
    }
    let mut shuffled = input.clone();
    shuffled["trees"] = Value::Array(shuffled_vec);
    assert_eq!(
        clades::run(&serde_json::to_string(&shuffled).unwrap()).unwrap(),
        baseline
    );

    // Permute the leaves array as well.
    let mut leaves = input["leaves"].as_array().unwrap().clone();
    leaves.reverse();
    let mut relabeled = input.clone();
    relabeled["leaves"] = Value::Array(leaves);
    assert_eq!(
        clades::run(&serde_json::to_string(&relabeled).unwrap()).unwrap(),
        baseline
    );
}

#[test]
fn unanimous_trees_give_full_support_and_original_shape() {
    let pectinate = json!(["a", ["b", ["c", "d"]]]);
    let input = json!({
        "leaves": ["a", "b", "c", "d"],
        "trees": [
            {"weight": 1, "tree": pectinate.clone()},
            {"weight": 2, "tree": pectinate.clone()},
            {"weight": 3, "tree": pectinate.clone()}
        ]
    });
    let out = tool_output(&input);
    let groups = out["groups"].as_array().unwrap();
    assert_eq!(groups.len(), 2, "only the two non-trivial clades");
    for group in groups {
        assert_eq!(group["fraction"], json!("1/1"));
        assert_eq!(group["accepted"], json!(true));
        assert!(group.get("conflict_with").is_none());
    }
    assert_eq!(out["consensus_tree"], pectinate);

    let balanced = json!([["a", "b"], ["c", "d"]]);
    let input = json!({
        "leaves": ["d", "c", "b", "a"],
        "trees": [
            {"weight": 4, "tree": balanced.clone()},
            {"weight": 6, "tree": balanced.clone()}
        ]
    });
    let out = tool_output(&input);
    for group in out["groups"].as_array().unwrap() {
        assert_eq!(group["fraction"], json!("1/1"));
        assert_eq!(group["accepted"], json!(true));
    }
    assert_eq!(out["consensus_tree"], balanced);
}

fn base_input() -> Value {
    json!({
        "leaves": ["a", "b", "c"],
        "trees": [
            {"weight": 1, "tree": ["a", ["b", "c"]]},
            {"weight": 2, "tree": ["b", ["a", "c"]]}
        ]
    })
}

#[test]
fn invalid_inputs_are_rejected() {
    let mut cases: Vec<(Value, &str)> = Vec::new();

    let mut v = base_input();
    v["leaves"] = json!(["a", "b"]);
    cases.push((v, "3..=20 leaves"));

    let mut v = base_input();
    v["leaves"] = json!(["a", "a", "b"]);
    cases.push((v, "duplicate leaf name"));

    let mut v = base_input();
    v["leaves"] = json!(["a", "b", "é"]);
    cases.push((v, "ASCII"));

    let mut v = base_input();
    v["leaves"] = json!(["a", "b", ""]);
    cases.push((v, "non-empty ASCII"));

    let mut v = base_input();
    v["trees"] = json!([{"weight": 1, "tree": ["a", ["b", "c"]]}]);
    cases.push((v, "2..=30 trees"));

    let mut v = base_input();
    v["trees"][0]["weight"] = json!(0);
    cases.push((v, "weight must be in 1..=10"));

    let mut v = base_input();
    v["trees"][0]["weight"] = json!(11);
    cases.push((v, "weight must be in 1..=10"));

    let mut v = base_input();
    v["trees"][0]["tree"] = json!(["a", "b"]);
    cases.push((v, "missing leaf"));

    let mut v = base_input();
    v["trees"][0]["tree"] = json!([["a", "a"], "b"]);
    cases.push((v, "appears 2 times"));

    let mut v = base_input();
    v["trees"][0]["tree"] = json!(["a", ["b", "z"]]);
    cases.push((v, "unknown leaf"));

    let mut v = base_input();
    v["trees"][0]["tree"] = json!([["a", "b", "c"], "a"]);
    cases.push((v, "exactly 2 children"));

    let mut v = base_input();
    v["trees"][0]["tree"] = json!(["a", ["b", 1]]);
    cases.push((v, "leaf-name strings"));

    for (input, needle) in cases {
        let error = clades::run(&serde_json::to_string(&input).unwrap()).unwrap_err();
        assert!(
            error.to_string().contains(needle),
            "error {error:?} should contain {needle:?}"
        );
    }
}

#[test]
fn cli_emits_report_on_stdout() {
    let input = json!({
        "leaves": ["a", "b", "c"],
        "trees": [
            {"weight": 1, "tree": ["a", ["b", "c"]]},
            {"weight": 2, "tree": ["a", ["b", "c"]]}
        ]
    });
    let mut child = Command::new(env!("CARGO_BIN_EXE_clades"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(serde_json::to_string(&input).unwrap().as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success(), "stderr: {:?}", output.stderr);
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["consensus_tree"], json!(["a", ["b", "c"]]));
    assert_eq!(report["groups"][0]["fraction"], json!("1/1"));
}

#[test]
fn cli_reports_errors_on_stderr() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_clades"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(br#"{"leaves": ["a", "b"], "trees": []}"#)
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(!output.status.success());
    let stderr: Value = serde_json::from_slice(&output.stderr).unwrap();
    assert!(stderr["error"].as_str().unwrap().contains("3..=20 leaves"));
}
