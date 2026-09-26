# Weighted Clade Consensus Builder

Different researchers rarely draw the same bifurcating tree for one set of
taxa, and naively merging every popular split into a single tree produces
impossible nestings. `clades` is a JSON command-line tool that merges
**weighted rooted binary trees** over a shared leaf set into a single,
possibly **multifurcating consensus tree**.

## Usage

The tool reads one JSON document from **stdin** and writes the JSON report to
**stdout**. Invalid input yields `{"error": "..."}` on stderr and exit code 1.

```sh
cargo run --release < examples/input.json
```

Or via the Compose `clades` service:

```sh
docker compose build clades
docker compose run --rm -T clades < examples/input.json
```

## Input

```json
{
  "leaves": ["a", "b", "c", "d", "e"],
  "trees": [
    { "weight": 5, "tree": [["a", "b"], ["c", ["d", "e"]]] },
    { "weight": 3, "tree": [["c", "d"], ["a", ["b", "e"]]] }
  ]
}
```

- `leaves`: 3–20 unique, non-empty ASCII names.
- `trees`: 2–30 rooted binary trees as nested arrays — a leaf is a string,
  an internal node is a 2-element array of children.
- `weight`: integer 1–10 per tree.
- Every tree must contain each leaf **exactly once**.

## Algorithm

1. **Support extraction.** Every non-trivial leaf set (2 to n−1 leaves, i.e.
   every clade except singletons and the root) is collected from each tree;
   its *support* is the sum of the weights of the trees containing it.
2. **Ranking.** Candidate clades are ordered by support descending, then set
   size ascending, then the sorted leaf-name list lexicographically.
3. **Greedy laminar selection.** Walking the ranking, a clade is *accepted*
   if it is compatible with every already-accepted clade (two sets are
   compatible when they are disjoint or one contains the other). A rejected
   clade records the **first conflicting accepted clade** as
   `conflict_with`.
4. **Consensus rebuild.** The accepted family is laminar, so it nests into a
   tree: each clade's parent is the smallest accepted clade strictly
   containing it (otherwise the root). Nodes may be multifurcating. Each
   node's children are sorted by their minimum leaf name. Every clade's
   support is reported as a reduced fraction `support / total_weight`.

## Output

```json
{
  "leaves": ["a", "b", "c", "d", "e"],
  "tree_count": 2,
  "total_weight": 8,
  "groups": [
    { "leaves": ["a", "b"], "support": 5, "fraction": "5/8", "accepted": true },
    { "leaves": ["c", "d"], "support": 3, "fraction": "3/8",
      "accepted": false, "conflict_with": ["d", "e"] }
  ],
  "consensus_tree": [["a", "b"], ["c", ["d", "e"]]]
}
```

- `groups`: every candidate clade in processing order, with its raw support,
  reduced support fraction, acceptance status, and — for rejected clades —
  the first conflicting accepted clade.
- `consensus_tree`: nested arrays in the same shape as the input trees
  (internal nodes may have more than two children).

## Development

```sh
cargo test
```

The integration tests enumerate leaf subsets of small trees independently
(deciding clade membership via lowest common ancestors in a separate arena
representation) and cross-check the extracted supports; they also verify the
conflict witnesses, invariance under permuting the input trees and leaf list,
the unanimous full-support case, multifurcation, input validation, and the
CLI's stdin/stdout contract.
