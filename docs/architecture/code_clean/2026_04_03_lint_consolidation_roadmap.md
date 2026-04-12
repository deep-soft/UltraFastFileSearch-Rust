# Lint Consolidation Roadmap

**Created:** 2026-04-03
**Updated:** 2026-04-11
**Status:** Phase 2A ✅, Phase 2B ✅, Phase 2C ✅, Phase 2D ✅ (accepted by design)

---

## Current State (2026-04-11)

All 13 crates inherit `[lints] workspace = true` with **no crate-level overrides**.
The workspace `Cargo.toml` sets `pedantic`, `nursery`, and `cargo` groups at **deny**
level. The full workspace passes clean:

```bash
cargo clippy --workspace --lib --bins --no-deps -- -D warnings   # ← zero errors
cargo test --workspace                                            # ← all tests pass
```

No crate has its own `[lints.clippy]` section — the workspace config is the single
source of truth.

---

## Completed Work

### Phase 1: Workspace Inheritance (done prior to 2026-04-03)

All crates migrated to `[lints] workspace = true`.

### Phase 2A: Fix the bulk lints in `uffs-mft` (done 2026-04-11)

The `uffs-mft` crate had `pedantic = "allow"` and `nursery = "allow"` because
low-level NTFS parser code triggered hundreds of pedantic lints. Those crate-level
overrides were removed and every lint violation was fixed at the root cause:

| Lint | Original Count | How Fixed |
|------|----------------|-----------|
| `indexing_slicing` | ~278 | Converted `rd_u16`/`rd_u32`/`rd_u64` and `decode_utf16le_into` helpers to safe `.get()` + `try_from` patterns. Remaining per-expression `#[expect]` attributes (8 total) are scoped to individual functions with `reason` strings documenting the bounds guarantee. |
| `default_numeric_fallback` | ~43 | Added explicit type suffixes (`0_u32`, `8_u8 << 2_u8`, etc.) |
| `min_ident_chars` / `single_char_binding_names` | ~42 | Renamed: `c` → `pair`, `b` → `bytes_f64`, `e` → `err`/`ext`, `i` → `idx`, `a` → `alloc_size` |
| `shadow_unrelated` / `shadow_reuse` | ~23 | Renamed shadowed bindings (`record` → `rec_link`/`rec_stream`/`rec_data`/etc.), snapshotted fields into `PreprocessSnapshot` struct to avoid re-borrowing, restructured into separate scopes |
| `cognitive_complexity` | 25 newly exposed | 9 fixed by extracting helpers (see below); 16 pre-existing parser/pipeline functions retain scoped `#[expect]` — accepted by design (see Phase 2D). |
| `unused_crate_dependencies` | 6 | Fixed incorrect `#[cfg(windows)]` / `#[cfg(test)]` gates on `use X as _` in `main.rs`; added missing crate references for `bytemuck`, `uffs_security`, `uffs_text` |
| `float_arithmetic` | ~10 | Per-expression `#[expect]` with `reason` — display-only formatting (MB conversion, ratios) |
| `collapsible_if` | 2 | Collapsed into `if let` chains |
| Other pedantic/nursery | ~47 | Fixed individually — numeric casts, doc comments, etc. |

#### Cognitive Complexity — 9 Functions Fixed by Extraction

Nine functions that were newly over the threshold (after `nursery = "allow"` was
removed) were brought under the complexity limit by extracting focused helpers:

| Function | Score | Helpers Extracted |
|----------|-------|-------------------|
| `TreeTraversal::preprocess` (tree_metrics.rs) | 55 | → `snapshot_record`, `aggregate_children`, `store_printed_metrics` + `PreprocessSnapshot` struct |
| `TreeTraversal::run` (tree_metrics.rs) | 55 | → `traverse_from_root`, `sweep_orphans` |
| `generate_read_chunks` (chunking.rs) | 42 | → `split_extent_into_chunks`, `log_bitmap_diagnostic` |
| `generate_precise_read_chunks` (chunking.rs) | 31 | → `split_extent_into_precise_chunks`, `emit_io_chunks` |
| `compute_tree_metrics` (tree_metrics.rs) | 31 | → `warn_unstamped_directories` |
| `migrate_legacy_cache` (cache.rs) | 30 | → `migrate_single_file` |
| `rebuild_children_from_names` (child_order.rs) | 29 | → `collect_parent_child_edges` |
| `add_missing_parent_placeholders` (columns.rs) | 26 | → `insert_missing_parent_round` |
| `add_missing_parent_placeholders_to_vec` (placeholders.rs) | 26 | → `insert_missing_parents` |
| `MftExtentMap::new` (extent_map.rs) | 26 | → `log_extent_layout`, `log_extent_details` |

### Phase 2B: Raise workspace level to deny (done 2026-04-11)

The workspace `Cargo.toml` now has:

```toml
[workspace.lints.clippy]
cargo = { level = "deny", priority = -1 }
nursery = { level = "deny", priority = -1 }
pedantic = { level = "deny", priority = -1 }
```

`cognitive_complexity` and `too_many_lines` are set to **warn** level (not deny)
as they are advisory — exceeding the threshold is flagged but genuine algorithmic
complexity is resolved by extraction, not suppression.

### Phase 2C: Remove CI flag duplication (done 2026-04-11)

Removed redundant `-D clippy::pedantic -D clippy::nursery -D clippy::cargo` and
`-A clippy::multiple_crate_versions` flags from all four lint commands in
`scripts/ci/ci-pipeline.rs`. These are now set in the workspace `Cargo.toml` —
the CI commands only pass `-D warnings` plus per-target overrides:

- `-W clippy::panic`, `-W clippy::todo`, `-W clippy::unimplemented` — relax from
  deny to warn (catches stray panics without failing the build)
- `-A clippy::unwrap_used`, `-A clippy::expect_used` — allow in test lint only
- `-A unused-crate-dependencies` — allow in test lint only

### Phase 2D: Remaining `cognitive_complexity` suppressions (accepted by design)

16 functions in `uffs-mft` and 21 in other crates carry
`#[expect(clippy::cognitive_complexity)]` with `reason` strings.
These are **accepted as intentional design** — not tech debt.

**Why these stay monolithic:**

The NTFS MFT parsers dispatch on 10+ attribute type codes
(`$STANDARD_INFORMATION`, `$FILE_NAME`, `$DATA`, `$INDEX_ROOT`,
`$ATTRIBUTE_LIST`, etc.) in a single `match` per record. Clippy's
cognitive-complexity heuristic penalises this heavily, but the code is
**linear** — each arm is independent, order doesn't matter beyond the
match, and a developer reading "what happens when we hit attribute type
0x30?" finds the answer in one place.

Splitting these into per-attribute handler functions or a trait-based
dispatch pattern would:

- **Scatter tightly-coupled logic** across 10+ files/functions, forcing
  a reader to jump between them to understand a single record parse.
- **Add indirection overhead** in the hottest path of the application
  (MFT parsing processes millions of records per second).
- **Increase maintenance cost** — adding a new attribute type currently
  means adding one `match` arm; with dispatch it means a new file, a
  trait impl, registration in a table, and wiring.
- **Produce a "clippy-compliant but worse" codebase** — the opposite of
  what linting is for.

The same reasoning applies to the daemon/MCP/CLI handler functions: they
are request dispatchers where the full request→response flow is most
readable as a single function.

**Inventory (uffs-mft, 16 functions):**

| File | Score | Function |
|------|-------|----------|
| `index/tree.rs` | 92 | `compute_tree_metrics_impl` |
| `index/builder.rs` | 83 | `build_from_index` |
| `parse/direct_index_extension.rs` | 82 | extension parser closure |
| `io/parser/index_extension.rs` | 80 | `parse_extension_to_index` |
| `reader/persistence.rs` | 79 | `load_raw_to_index_with_options` |
| `raw_iocp.rs` | 69 | `load_iocp_to_index` |
| `parse/direct_index.rs` | 69 | direct index parser closure |
| `io/parser/index.rs` | 68 | `parse_record_to_index` |
| `parse/full.rs` | 54 | `parse_record_full` |
| `index/storage/deserialize.rs` | 52 | deserializer |
| `io/readers/parallel/tests_chaos.rs` | 52 | `ChaosMftReader::read_with_chaos` |
| `parse/forensic/base.rs` | 44 | forensic base parser |
| `index/merge.rs` | 44 | `merge_fragments` |
| `io/parser/fragment.rs` | 41 | fragment parser |
| `io/parser/unified.rs` | 38 | unified parser |
| `parse/forensic/extension.rs` | 33 | forensic extension parser |

**Other crates (21 functions):** `uffs-daemon` (10), `uffs-mcp` (3),
`uffs-core` (3), `uffs-cli` (2), `uffs-tui` (1). Same pattern — request
dispatchers and orchestration functions where the full flow belongs
together.

---

## Scoped `#[expect]` Policy

All remaining `#[expect]` attributes in the workspace follow these rules:

1. **Narrowest possible scope** — per-expression or per-function, never per-module
2. **Mandatory `reason` string** — explains why the suppression is necessary
3. **`#[expect]` over `#[allow]`** — `expect` warns if the lint stops firing
   (the suppression becomes dead code and can be removed)
4. **No `#[allow(clippy::...)]` in production code** — only `#[expect]`

### Remaining `#[expect]` inventory (workspace-wide, 2026-04-11)

**`uffs-mft` (46 `#[expect]` attributes):**

| Lint | Count | Justification |
|------|-------|---------------|
| `cognitive_complexity` | 16 | NTFS parser/pipeline functions — accepted by design (see Phase 2D) |
| `float_arithmetic` | 10 | Display-only formatting (MB conversion, ratios) |
| `indexing_slicing` | 8 | Per-function with bounds-guarantee documentation |
| `too_many_lines` | 3 | Monolithic NTFS pipelines |
| `too_many_arguments` | 5 | Pass-through context to extracted helpers |
| `print_stdout` | 5 | CLI binary output |
| Other | 6 | `unused_async`, `partial_pub_fields`, `min_ident_chars`, etc. |

**Other crates (combined ~65 `#[expect]` attributes):**

| Lint | Count | Crates | Justification |
|------|-------|--------|---------------|
| `unwrap_used` | 35 | test code | Tests use unwrap for assertion-style failures |
| `single_call_fn` | 32 | all | Extracted for clarity despite single call site |
| `print_stdout` | 23 | cli, daemon | Intentional CLI output |
| `cognitive_complexity` | 21 | daemon, mcp, core, cli, tui | Accepted by design — dispatchers and orchestrators (see Phase 2D) |
| `expect_used` | 13 | test/setup code | Infallible setup where failure is a bug |
| Other | ~30 | various | Individually justified |

Total: ~111 scoped `#[expect]` across the entire workspace.
Zero `#[allow(clippy::...)]` in production code.

