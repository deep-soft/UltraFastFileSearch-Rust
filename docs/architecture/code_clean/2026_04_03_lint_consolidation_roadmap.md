# Lint Consolidation Roadmap

**Created:** 2026-04-03  
**Status:** Phase 1 complete (workspace inheritance), Phase 2 pending (full pedantic compliance)

---

## Current State

All crates now inherit `[lints] workspace = true`. The workspace sets `pedantic` and
`nursery` at **warn** level, and the CI pipeline enforces them as errors via
`-D clippy::pedantic -D clippy::nursery` flags. This is functionally equivalent to
deny-everywhere for CI, but doesn't block local development.

### Why `uffs-mft` was the holdout

The `uffs-mft` crate had its own `[lints.clippy]` with `pedantic = allow` and
`nursery = allow` because low-level NTFS MFT parser code naturally triggers many
pedantic lints:

| Lint | Count | Why it fires |
|------|-------|--------------|
| `indexing_slicing` | 278 | Raw byte buffer parsing with pre-validated bounds |
| `default_numeric_fallback` | 43 | Integer literals in binary format parsing |
| `single_char_binding_names` | 42 | `i`, `n`, `b` in tight parser loops |
| `shadow_unrelated` | 23 | Re-binding variables in sequential parse stages |
| `cognitive_complexity` | 2 | Monolithic NTFS attribute type dispatchers |

These are not bugs — they're deliberate patterns in performance-critical parser code.

---

## Goal: Full Pedantic Compliance (Option 1)

The long-term goal is to raise `pedantic` and `nursery` back to **deny** in the
workspace `Cargo.toml` with all crates passing clean. This gives:

1. **Uniform code quality** — every crate held to the same standard
2. **No CI/config divergence** — lint levels live in one place, not split between
   `Cargo.toml` and CI flags
3. **Local `cargo clippy` matches CI** — developers catch issues before pushing

---

## How To Do It Properly

### Phase 2A: Fix the bulk lints (~1–2 days)

Work through `uffs-mft` lint errors by category, largest first:

1. **`indexing_slicing` (278):** For each site, decide:
   - **Replace with `.get()`** if the bounds aren't statically obvious
   - **Add a local `#[allow(clippy::indexing_slicing)]`** with a `// SAFETY:` comment
     if bounds are validated by a preceding assert/check/slice length guarantee.
     This is NOT a suppression hack — it's documenting the invariant.
   - **Restructure** tight loops to use iterators or `chunks_exact()` where possible

2. **`default_numeric_fallback` (43):** Add explicit type suffixes to all bare integer
   literals (`0` → `0_u32`, `1` → `1_usize`, etc.). Mechanical — use
   `cargo clippy --fix` with `--allow-dirty`.

3. **`single_char_binding_names` (42):** Rename variables to descriptive names
   (`i` → `idx`, `b` → `byte`, `n` → `count`). Do this per-function.

4. **`shadow_unrelated` (23):** Rename shadowed variables to distinct names or
   restructure into separate scopes. Requires reading each function.

5. **Remaining (~47):** Address individually — most are mechanical fixes.

### Phase 2B: Raise workspace level back to deny

Once all uffs-mft lints pass:

```toml
# Cargo.toml
[workspace.lints.clippy]
cargo = { level = "deny", priority = -1 }
nursery = { level = "deny", priority = -1 }   # ← back to deny
pedantic = { level = "deny", priority = -1 }   # ← back to deny
```

Remove the corresponding `-D clippy::pedantic -D clippy::nursery` from CI pipeline
flags (they become redundant since the workspace config enforces them).

### Phase 2C: Remove CI flag duplication

Once workspace `Cargo.toml` is the single source of truth, simplify CI lint commands:

```bash
# Before (flags duplicate workspace config):
cargo clippy --workspace --lib --bins --no-deps -- \
  -D clippy::pedantic -D clippy::nursery -D clippy::cargo -D warnings

# After (workspace config is authoritative):
cargo clippy --workspace --lib --bins --no-deps -- -D warnings
```

The `-D warnings` alone is sufficient because the workspace lint config already
sets the correct levels.

### Validation

After each phase, run the full pipeline:
```bash
just ship -v
```

---

## Scoped `#[allow]` Policy

When fixing `indexing_slicing` in parser code, targeted `#[allow]` attributes are
acceptable **if and only if**:

1. The allow is on the narrowest possible scope (one expression or function, not a module)
2. A comment explains why the index is safe (what guarantees bounds)
3. The pattern is genuinely performance-critical (hot path in MFT parsing)

Example:
```rust
// Bounds guaranteed: `offset + 8 <= buf.len()` checked at line 142
#[allow(clippy::indexing_slicing)]
let value = u64::from_le_bytes(buf[offset..offset + 8].try_into()?);
```

This is NOT a suppression hack — it's documenting a verified invariant.

