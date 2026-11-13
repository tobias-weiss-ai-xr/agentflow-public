# Module map format (ADR-16)

A **module map** is a static JSON snapshot of a repo's code structure — files,
function declarations, and call edges — produced by an external tool and
consumed by `af validate` to warn when two tasks' scopes are call-connected
(semantic contention that text globs cannot see).

The feature is **optional**: absent the declaration, the check is silently
off; stale or invalid maps produce a WARNING and disable the check, but never
block validation (never a hard error).

---

## Schema

The module map is a single JSON object with three top-level keys:

```json
{
  "files":     string[],           // repo-relative file paths
  "functions": [ FunctionDecl ],   // function / method declarations
  "calls":     [ CallEdge ]        // directed call edges (deduped)
}
```

### `FunctionDecl`

| Field  | Type   | Meaning                                      |
|--------|--------|----------------------------------------------|
| `name` | string | Function or method name (unqualified)        |
| `path` | string | Repo-relative path of the file that declares it |
| `line` | number | 1-based line number of the declaration        |

### `CallEdge`

| Field  | Type   | Meaning                                          |
|--------|--------|--------------------------------------------------|
| `from` | string | Repo-relative path of the **caller** file        |
| `to`   | string | Repo-relative path of the **callee** file        |

Edges are **file-level**: `from` and `to` are repo-relative *file* paths, not
function names. Direction matters — `(A → B)` is distinct from `(B → A)`.
Duplicate edges should be deduped, but the consumer treats them as a set
anyway.

---

## Declaration in tasks.json

A campaign opts in by adding a top-level `"module_map"` key to `tasks.json`:

```json
{
  "module_map": "module-map.json",
  "tasks": [ … ]
}
```

- The value is a **path** resolved against the directory containing the
  `tasks.json` file (i.e. `PathBuf::from(parent).join(value)`).
- If the path is absent or the file does not exist → **WARNING** on stderr,
  semantic-contention check disabled, validation continues.
- If the file exists but fails JSON parsing or schema validation →
  **WARNING** on stderr, check disabled, validation continues.
- If the key is omitted entirely → feature silently off, no warning.

---

## Semantic-contention warning

When `af validate` loads a valid module map, it checks every pair of tasks
whose `scope` globs each match at least one file. If the call graph contains
an edge `(A → B)` where task X's scope matches `A` and task Y's scope matches
`B` (or vice versa), the two tasks are **call-connected** — a change in one
can break the other even if their textual scopes don't overlap.

For each such pair, `af validate` emits a warning:

```
warning: semantic contention: tasks <X> and <Y> are call-connected
         via <A> → <B> (module map)
```

The warning is informational; it does not prevent `af run` from dispatching
both tasks. It signals that the orchestrator or a human may want to add a
dependency (`"deps": ["<X>"]` on task Y) to serialize the work and avoid a
merge conflict.

---

## Generating a module map

The module map is produced **outside** af — af embeds no code-graph database
or static analyser (ADR-1: deterministic, thin, zero runtime deps). Any tool
that can enumerate files, function declarations, and call edges and export
them in the JSON shape above will work.

### Example: cgc / CodeGraphContext

[CodeGraphContext](https://github.com/context-labs/cgc) (cgc) builds a
SQLite call-graph from a repo and can query it from the CLI:

```sh
# Build the database (once per repo revision)
cgc --path . build

# Export a module map
cgc --path . query --format json \
    --select files,functions,calls \
    > module-map.json
```

The exported JSON must be reshaped to match the schema above (cgc's native
output may use different key names). A thin wrapper script can do the
translation; the result is checked into the campaign directory alongside
`tasks.json`.

### Other tools

Any combination of static analysis that yields `(file, function, line)` and
`(caller_file, callee_file)` pairs is valid:

- **tree-sitter** queries over the repo's language grammars.
- **Doxygen + Graphviz** dot parsing for C/C++ projects.
- **cargo + rust-analyzer** or **pylint / pyreverse** for Rust / Python.
- A hand-written map for small repos.

The only contract is the JSON schema.

---

## Freshness

A module map is a point-in-time snapshot. When the repo changes (files added/
removed, call edges rewired), the map becomes stale. This is **not a hard
error**: `af validate` will warn and disable the semantic-contention check,
then continue. The campaign author is responsible for regenerating the map
when the repo structure changes significantly.

A recommended practice is to regenerate the map in the same CI step that
runs `af validate`, so the map is always fresh. If that is not possible,
treat the map as advisory — its warnings are hints, not gates.

---

## Minimal example

See [`examples/module-map.example.json`](../examples/module-map.example.json)
for a complete, valid instance with realistic paths from af's own source
tree.
