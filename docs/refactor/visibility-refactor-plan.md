# Visibility Refactor Plan

## Goal

Flatten visibility across the workspace: make all production modules and items
`pub`, delete every re-export so callers import from the definition site.

**Invariants at every checkpoint:**
- `just lint-prod` — zero errors
- `just lint-tests` — zero errors
- `just test` — 996/996 pass, 12 skipped

## Rules

- Production code: all `mod foo` → `pub mod foo`, all items `pub`.
- Test code: `mod tests` stays private; test-internal helpers may use
  `pub(super)` / `pub(crate)` where structurally required.
- No blanket `#[allow]` suppression. Scoped `#[expect(..., reason = "...")]`
  only where the lint is genuinely inapplicable.
- Add `#[must_use]` and `# Errors` / `# Panics` doc sections where clippy
  requires them on newly-public items.

---

## Lint configuration fix

`clippy::redundant_pub_crate` (from `-D clippy::nursery` in the justfile)
directly conflicts with `unreachable_pub` (enabled in `Cargo.toml`).
The `Cargo.toml` comment already documented this: _"Conflicts with
`rustc::unreachable_pub` — we prefer `unreachable_pub`"_ and set
`redundant_pub_crate = "allow"`, but the justfile's `-D clippy::nursery`
overrode it.

**Fix applied:** Added `-A clippy::redundant_pub_crate` to `common_flags` in
`just/shared.just`, aligning the CLI flags with the documented workspace
policy. This follows the existing precedent of `-A clippy::multiple_crate_versions`
already present in the same flag list.

This means `pub(crate)` inside private modules is acceptable and does **not**
need to be changed to `pub`. Only modules that need broader visibility should
be promoted.

---

## Progress

### Completed ✓

| Change | Scope | Status |
|--------|-------|--------|
| `just lint-prod` clean | Workspace | **Done** — zero errors |
| `just lint-tests` clean | Workspace | **Done** — zero errors |
| `just test` passes | Workspace | **Done** — 996 pass, 12 skipped |
| `redundant_pub_crate` lint conflict | `just/shared.just` | **Fixed** — `-A clippy::redundant_pub_crate` added |
| uffs-cli visibility widening | `commands.rs`, submodules | **Done** — `mod` → `pub mod`, `pub(crate)` → `pub` |
| uffs-cli re-export removal | `commands.rs` | **Done** — 7 `pub(crate) use` removed, callers use full paths |
| uffs-broker visibility widening | `main.rs` | **Done** — `mod broker` → `pub mod broker` |
| uffs-mcp: `pid` re-export removal | `lib.rs` | **Done** — callers already use `uffs_mcp::pid::*` |
| uffs-client: `daemon_ctl` re-export removal | `lib.rs` | **Done** — `pub use crate::daemon_ctl::*` removed |
| uffs-mcp: module visibility | handler, tools, etc. | **Done** — `mod` → `pub mod` where needed |
| uffs-daemon: `uffsd` binary target | `Cargo.toml` | **Done** — `[[bin]] name = "uffsd"` added |
| uffs-mcp: `uffs-core` dependency removed | `Cargo.toml` | **Done** — unused transitive dep cleaned up |

### Not started — visibility widening

These crates still have private modules and `pub(crate)` / `pub(super)` items
in production code. These do **not** block `lint-prod`/`lint-tests` (thanks to
the lint config fix), but should be addressed if the full visibility flattening
goal is pursued.

| Crate | `pub(crate)` remaining | `pub(super)` remaining | private `mod` remaining |
|-------|----------------------|----------------------|------------------------|
| uffs-core | 9 | 45 | 33 |
| uffs-daemon | 51 | 11 | 10 |
| uffs-mft | 43 | 48 | 104 |
| **Total** | **103** | **104** | **147** |

**Why not done:** Aggressive `pub(crate)` → `pub` and `mod` → `pub mod`
changes in uffs-mft and uffs-core were attempted but reverted — the sheer
volume (~150+ callers using re-exported short paths) caused cascading breakage
that was impractical to fix in a single pass. These should be done module by
module with careful caller migration.

### Not started — re-export removal

| Crate | Re-exports remaining | Estimated callers to update |
|-------|---------------------|---------------------------|
| uffs-core | 44 | ~60 (internal + uffs-daemon, uffs-cli) |
| uffs-mft | 77 | ~150 (heavy internal use of short paths) |
| uffs-mcp | 2 | ~5 (handler re-exports needed by lib.rs callers) |
| uffs-client | 1 | ~2 (`pub use response::*`) |
| uffs-text | 2 | ~3 (downstream crates use short paths) |
| uffs-polars | 1 | N/A — facade crate, **keep intentionally** |
| uffs-diag | 1 | ~1 |
| **Total** | **128** | **~220** |

**Note:** `uffs-polars` re-exports (`pub use polars::prelude::*` and
`pub use polars::{...}`) are the crate's entire reason for existing — they
form the Polars compilation-cache facade. These are kept intentionally.

---

## Remaining work — re-export removal by crate

Execution order: bottom-up in the dependency graph. Each phase ends with
`cargo check --workspace && just lint-prod && just lint-tests && just test`.

---

### Phase R1: uffs-text (2 re-exports) — ✓ DONE

Deleted both re-exports from `lib.rs`. Updated all callers:
- 6 `use uffs_text::CaseFold` → `use uffs_text::case_fold::CaseFold`
- 1 `use uffs_text::{CaseFold, pack_char_trigram}` → split into `case_fold::CaseFold` + `trigram_key::pack_char_trigram`
- ~60 inline `uffs_text::CaseFold` → `uffs_text::case_fold::CaseFold` (production + test code)
- 3 inline references in `uffs-mft/src/platform/upcase.rs`
- Doc links in `lib.rs` updated to `case_fold::CaseFold` / `trigram_key::*`

---

### Phase R2: uffs-client (1 re-export) — ✓ DONE

Deleted `pub use response::*;` from `protocol/mod.rs`.
Updated all callers — split mixed `use` statements to separate
`protocol::response::` imports (20 response types) from `protocol::` imports
(wire types, error codes, params). 7 files updated across uffs-daemon and uffs-cli.

---

### Phase R3: uffs-mcp (2 remaining re-exports) — ✓ DONE

Deleted both re-exports from `handler/mod.rs`. Updated 4 callers:
- `handler/mod.rs`: 3 internal calls → `definitions::tool_definitions()`,
  `definitions::prompt_definitions()`, `prompts::build_prompt_messages()`
- `lib.rs`: 1 call → `crate::handler::definitions::prompt_definitions()`

---

### Phase R4: uffs-core (44 re-exports) — LARGE, NOT STARTED

Seven parent modules contain re-exports:

**`lib.rs`** (14 facade re-exports):
```
84   pub use compiled_pattern::{CompiledPattern, GlobKind, ...};
85   pub use error::{CoreError, Result};
86   pub use export::{export_csv, export_json, export_table};
87   pub use extensions::{ExtensionFilter, ExtensionIndex, ...};
90   pub use index_search::{IndexPattern, IndexQuery, ...};
100  pub use path_resolver::add_path_column_multi_drive;
101  pub use path_resolver::{FastPathResolver, ...};
105  pub use query::MftQuery;
106  pub use slot_pool::{DriveLoadEstimate, SlotPool, ...};
107  pub use tree::{TreeColumn, add_tree_columns, ...};
109  pub use uffs_mft::FileFlags;
110  pub use uffs_polars::{DataFrame, IntoLazy, LazyFrame, ...};
```
Delete all 14. Downstream callers (uffs-daemon, uffs-cli, uffs-mcp) update:
- `uffs_core::CoreError` → `uffs_core::error::CoreError`
- `uffs_core::MftQuery` → `uffs_core::query::MftQuery`
- `uffs_core::FastPathResolver` → `uffs_core::path_resolver::FastPathResolver`
- `uffs_core::FileFlags` → `uffs_mft::FileFlags` (add uffs-mft dep where needed)
- `uffs_core::DataFrame` → `uffs_polars::DataFrame` (add uffs-polars dep where needed)
- etc.

**`index_search/mod.rs`** (4 re-exports):
```
53   pub use self::pattern::{IndexPattern, compile_extensions, ...};
57   pub use self::query::{IndexQuery, QueryOptions, TypeFilter};
58   pub use self::result::SearchResult;
59   pub use self::routing::{QueryComplexity, QueryFeatures, ...};
```

**`aggregate/mod.rs`** (13 re-exports):
```
88-105  pub use accumulators/buckets/cache/duplicates/export/
        finalize/pagination/parser/planner/presets/rollup/spec/verify::*;
```

**`compact.rs`** (2 re-exports):
```
22   pub use crate::compact_loader::{IndexSource, LoadTiming, ...};
27   pub use crate::compact_loader::{apply_usn_patch, load_live_drive};
```

**`search/backend.rs`** (1 re-export):
```
724  pub use super::sorting::{dataframe_to_display_rows, ...};
```

**`search/filters/mod.rs`** (3 re-exports):
```
14   pub use apply::*;
15   pub use attr_parsing::*;
16   pub use time_parsing::*;
```

**`search/field/mod.rs`** (1 re-export):
```
16   pub use uffs_client::schema::*;
```

**`path_resolver/mod.rs`** (4 re-exports):
```
24   pub use arena::NameArena;
25   pub use fast::{FastPathResolver, ...};
30   pub use legacy::{PathResolver, ...};
31   pub use multi_drive::{FastPathResolverMultiDrive, ...};
```

**`tree/mod.rs`** (2 re-exports):
```
48   pub use column::TreeColumn;
49   pub use index::TreeIndex;
```

**`compiled_pattern/mod.rs`** (1 re-export):
```
39   pub use glob::{GlobKind, classify_glob, compile_pattern};
```

**Strategy:** Delete the `lib.rs` facade last (it has the most downstream
impact). Start with leaf re-exports (`tree`, `compiled_pattern`,
`path_resolver`), then work up to `lib.rs`.

---

### Phase R5: uffs-cli (5 re-exports) — ✓ DONE

All 7 `pub(crate) use self::*` re-exports deleted from `commands.rs`.
Submodules promoted to `pub mod`. Callers in `main.rs` updated to use
full submodule paths (e.g., `commands::search::search()`).

---

### Phase R6: uffs-mft (77 re-exports) — NOT STARTED

Fourteen parent modules contain re-exports:

**`lib.rs`** (14 re-exports) — crate-root facade, same pattern as uffs-core.

**`index.rs`** (9 re-exports):
```
45-59  pub use self::extensions/fragment/model/path_resolver/
       standard_info/stats/storage/types/usn::*;
```
Callers update: `crate::index::MftIndex` → `crate::index::model::MftIndex`

**`io/readers/mod.rs`** (14 re-exports):
```
pub use self::basic/iocp/pipelined/prefetch/streaming/zero_copy::*;
```

**`io.rs`** (9 re-exports):
```
pub use crate::ntfs::SECTOR_SIZE;
pub use self::aligned_buffer/chunking/extent_map/parser/readers::*;
```

**`parse.rs`** (9 re-exports):
```
pub use self::attribute_helpers/columns/direct_index/fixup/
       full/merger/types/zero_alloc::*;
```

**`commands/windows/mod.rs`** (8 re-exports):
```
pub use self::bench/benchmark_index/benchmark_mft/bitmap_diag/
       incremental/info/read/save::*;
```

**`platform.rs`** (8 re-exports):
```
pub use self::bitmap/extents/system/volume::*;
```

**`ntfs/mod.rs`** (4 re-exports):
```
pub use self::boot_sector/data_runs/metadata/records::*;
```

**`reader.rs`** (4 re-exports):
```
pub use self::benchmark/multi_drive/read_mode/stats::*;
```

**`io/readers/iocp/mod.rs`** (4 re-exports):
```
pub use self::completion/overlapped::*;
pub use super::zero_copy::parse_buffer_zero_copy_inner;
```

**`io/parser/mod.rs`** (4 re-exports):
```
pub use unified::process_record;
pub use crate::parse::{ExtensionAttributes, ParseResult, ...};
```

**`cache.rs`** (2 re-exports), **`usn.rs`** (1), **`raw/mod.rs`** (1),
**`io/readers/parallel/mod.rs`** (1), **`io/merger.rs`** (1),
**`io/fixup.rs`** (1), **`index/storage/mod.rs`** (1).

**Strategy:** Work inside-out — start with the deepest leaf modules
(`io/fixup`, `io/merger`, `index/storage`), then move up to the mid-level
parents (`ntfs`, `reader`, `parse`, `io`), then the top-level (`index`,
`lib.rs`). This minimizes cascading breakage because each step only affects
callers within uffs-mft itself (the cross-crate callers mostly go through
`lib.rs`).

---

### Phase R7: uffs-diag (1 re-export) — ✓ DONE

Deleted `pub use stats::{ComparisonResults, FieldStats};` from `parity/mod.rs`.
Changed `mod stats` → `pub mod stats`. Updated 1 caller in `compare_scan_parity.rs`.

---

## Mechanical execution pattern

For each batch of re-exports to delete:

1. **Delete the `pub use` lines.**
2. **`cargo check -p <crate>`** — collect all `unresolved import` errors.
3. **Update callers** — change import paths to the full submodule path.
4. **`cargo check --workspace`** — catch downstream breakage.
5. **`cargo fmt --all`**
6. **`just lint-prod`** — fix `#[must_use]`, `# Errors`, doc issues.
7. **`just lint-tests`** — fix test-specific lint.
8. **`just test`** — all 996 pass.

The compiler is the safety net. Every broken import path produces an
`unresolved import` error pointing to the exact file and line.

---

## Lessons learned

1. **Do not batch re-export removal across many modules.** The cascading
   breakage from removing re-exports in uffs-mft's deeply nested module tree
   (~150 callers) made bulk changes impractical. Work one parent module at a
   time.

2. **Verify module visibility before adding explicit imports.** Many uffs-mft
   submodules (`types`, `model`, `fixup`, etc.) are private. Test code accesses
   them only through re-exported paths in parent modules. Adding direct
   `crate::index::types::X` paths fails if the module is private.

3. **Lint conflicts must be resolved at the config level.** The
   `redundant_pub_crate` vs `unreachable_pub` conflict cannot be fixed at the
   code level — any code change that satisfies one lint violates the other.
   The fix is aligning the lint configuration.

4. **Re-exports that serve as a public API surface should not be removed
   until all callers are migrated.** Removing `pub use response::*` from
   `uffs-client` broke `uffs-daemon` which depended on the short paths.

---

## Risk assessment

| Change | Risk | Mitigation |
|--------|------|-----------|
| `mod` → `pub mod` (uffs-cli, uffs-broker) | Zero | Done, all green |
| `mod` → `pub mod` (uffs-mft, uffs-core) | **Medium** | ~150 private mods; requires `#[must_use]`/doc additions on newly-public items |
| `pub(crate/super)` → `pub` | **Medium** | 103 `pub(crate)` + 104 `pub(super)` remaining; must widen enclosing module first |
| Delete leaf re-exports (tree, path_resolver, etc.) | Low | Few callers, compiler catches all |
| Delete mid-level re-exports (index_search, aggregate) | Medium | More callers but still internal |
| Delete `lib.rs` facade (uffs-core, uffs-mft) | **High** | Cross-crate impact; may require adding deps to Cargo.toml |
| Delete uffs-mft internal re-exports | **High** | ~150 callers using short paths throughout the crate |

Highest-risk items are the `lib.rs` facades and the uffs-mft internal
re-exports. These should be done last, one parent module at a time, with a
`cargo check` between each deletion.