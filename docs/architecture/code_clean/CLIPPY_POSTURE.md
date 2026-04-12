# Clippy Posture — Why UFFS Lints the Way It Does

UFFS runs one of the strictest Clippy configurations of any public Rust
project.  This document explains the philosophy, the tiers, the
trade-offs, and the mechanical rules that keep 13 crates clean under a
single shared lint config.

> **See also:** [Lint Consolidation Roadmap](2026_04_03_lint_consolidation_roadmap.md) ·
> [Casting Audit](CASTING_TRUNCATION_AUDIT.md)

---

## 1  The Two Files

All lint configuration lives in exactly two places:

| File | What it controls |
|------|-----------------|
| `Cargo.toml` `[workspace.lints]` | Which lints are enabled and at what level (`deny` / `warn` / `allow`) |
| `clippy.toml` | Numeric thresholds, test-mode relaxations, and behavioural toggles |

No individual crate has its own `[lints.clippy]` section.  Every crate
inherits `[lints] workspace = true`.  This is the single source of truth.

---

## 2  The Four Tiers

```
┌──────────────────────────────────────────────────────────────────┐
│  Tier 1: Lint Groups (deny)                                      │
│  pedantic · nursery · cargo — entire groups at deny level         │
├──────────────────────────────────────────────────────────────────┤
│  Tier 2: Restriction Lints (cherry-picked, deny)                 │
│  ~80 hand-picked lints from clippy::restriction                  │
│  NOT the whole group — each one chosen for a specific reason      │
├──────────────────────────────────────────────────────────────────┤
│  Tier 3: Rust Compiler Lints (deny/warn)                         │
│  unsafe_code · missing_docs · unreachable_pub · future_incompat  │
├──────────────────────────────────────────────────────────────────┤
│  Tier 4: Rustdoc Lints (deny/warn)                               │
│  broken links · invalid code blocks · bare URLs                  │
└──────────────────────────────────────────────────────────────────┘
```

### Why deny, not warn?

Warnings get ignored.  CI passes with warnings.  A `warn` that nobody
fixes for a month becomes a `warn` that nobody fixes ever.  We use
`-D warnings` in CI to promote every warning to an error, but setting
the lint level to `deny` directly makes the intent explicit in the
config file — you don't need to know the CI flags to understand the
contract.

The exceptions are lints where false positives exist but the signal is
still valuable: `unreachable_pub`, `elided_lifetimes_in_paths`,
`variant_size_differences`.  These stay at `warn`.

---

## 3  Production vs Test Code

UFFS treats production and test code as two different contexts with
different rules:

| Behaviour | Production | Tests |
|-----------|-----------|-------|
| `unwrap()` / `expect()` | **denied** — use `?` or `ok_or()` | **allowed** — tests should crash loudly |
| `panic!()` | **denied** | **allowed** |
| `dbg!()` | **denied** | **allowed** |
| `print!()` / `eprintln!()` | **denied** — use `tracing` | **allowed** |
| `assert!(r.is_ok())` | **denied** — use `r.unwrap()` | **denied** — same rule, use `r.unwrap()` for better diagnostics |
| `#[expect]` without reason | **denied** | **denied** |

This split is implemented via `clippy.toml`:

```toml
allow-unwrap-in-tests = true
allow-expect-in-tests = true
allow-panic-in-tests  = true
allow-dbg-in-tests    = true
allow-print-in-tests  = true
```

### The `#[expect]` rule

Every suppression must use `#[expect]` (not `#[allow]`) and must carry
a `reason = "..."` string:

```rust
// ✗ rejected — stale suppressions go unnoticed
#[allow(clippy::too_many_lines)]
fn big_function() { ... }

// ✗ rejected — no reason
#[expect(clippy::too_many_lines)]
fn big_function() { ... }

// ✓ accepted
#[expect(
    clippy::too_many_lines,
    reason = "NTFS attribute dispatch — 12 match arms, linear and readable"
)]
fn big_function() { ... }
```

`#[expect]` warns when the suppressed lint stops firing (the violation
was fixed but the attribute was left behind).  `#[allow]` silently
stays forever.  The `allow_attributes = "deny"` lint enforces this.

---

## 4  Restriction Lints — The Cherry-Pick Strategy

The `clippy::restriction` group contains ~120 lints.  Enabling the
entire group is impractical — many conflict with each other (e.g.
`implicit_return` vs `needless_return`).  The Rust community consensus,
followed by projects like Cargo, rust-analyzer, and Rolldown, is to
cherry-pick individually.

UFFS enables ~80 restriction lints, organised into 12 categories:

| Category | Count | Key lints |
|----------|-------|-----------|
| Error handling & safety | 10 | `unwrap_in_result`, `exit`, `mem_forget`, `try_err` |
| Memory & allocation | 6 | `rc_buffer`, `rc_mutex`, `significant_drop_tightening` |
| Concurrency | 3 | `mutex_atomic`, `stable_sort_primitive`, `infinite_loop` |
| Iterator patterns | 6 | `iter_over_hash_type`, `needless_collect`, `needless_for_each` |
| String & formatting | 5 | `format_push_string`, `string_lit_chars_any` |
| Numeric & types | 3 | `default_numeric_fallback`, `float_arithmetic`, `as_underscore` |
| Idiomatic patterns | 10 | `manual_let_else`, `option_if_let_else`, `equatable_if_let` |
| Code style | 16 | `min_ident_chars`, `shadow_*`, `similar_names`, `ref_patterns` |
| Output discipline | 3 | `print_stdout`, `print_stderr`, `use_debug` |
| Portability | 2 | `std_instead_of_core`, `std_instead_of_alloc` |
| Filesystem | 2 | `filetype_is_file`, `doc_include_without_cfg` |
| Meta / suppression | 4 | `allow_attributes`, `allow_attributes_without_reason` |

### What we intentionally do NOT enable

| Lint | Why skipped |
|------|-------------|
| `as_conversions` | Too noisy — `as` is idiomatic for infallible widening casts (`u32` → `u64`). We use `as_underscore` to catch only the dangerous inferred-type variant. |
| `arithmetic_side_effects` | Would require wrapping every `+` in `.checked_add()`. Not practical for index arithmetic. We rely on `default_numeric_fallback` + code review. |
| `absolute_paths` | Conflicts with `use` hygiene in large modules. |
| `implicit_return` | Conflicts with `needless_return`. Rust idiom is implicit returns. |
| `missing_inline_in_public_items` | We are not a published library — inlining is handled by LTO. |
| `single_call_fn` | Intentionally allowed — helper functions improve readability even if called once. |
| `redundant_pub_crate` | Conflicts directly with `unreachable_pub`. See §6 below. |
| `multiple_crate_versions` | Polars and tokio pull transitive version conflicts we cannot resolve. |

---

## 5  Rust Compiler Lints

Beyond Clippy, the `[workspace.lints.rust]` section enables several
rustc-level lints that Clippy cannot catch:

| Lint | Level | Why |
|------|-------|-----|
| `unsafe_code` | deny | No `unsafe` without explicit `#[allow(unsafe_code)]` + safety comment |
| `missing_docs` | deny | Every public item must be documented |
| `unsafe_op_in_unsafe_fn` | deny | Even inside `unsafe fn`, each operation must be in its own `unsafe {}` block |
| `future_incompatible` | deny | Catch breaking changes before the next edition |
| `unreachable_pub` | warn | Flag `pub` items in private modules — should be `pub(crate)` |
| `elided_lifetimes_in_paths` | warn | Make `Frame<'_>` explicit instead of `Frame` |
| `unused_lifetimes` | deny | Remove unnecessary lifetime parameters |
| `unused_import_braces` | deny | Clean import style |
| `unused_qualifications` | deny | Remove unnecessary path prefixes |

---

## 6  The `unreachable_pub` vs `redundant_pub_crate` Conflict

These two lints directly contradict each other:

```
unreachable_pub (rustc):
  "this `pub` in a private module is unreachable — use `pub(crate)`"

redundant_pub_crate (clippy nursery):
  "this `pub(crate)` is inside a private module — plain `pub` suffices"
```

You cannot satisfy both.  UFFS follows the Rust team's recommendation:
**prefer `unreachable_pub`** and suppress `redundant_pub_crate`:

```toml
# Cargo.toml
unreachable_pub = "warn"           # rustc lint — catches overly broad visibility
redundant_pub_crate = "allow"      # clippy — conflicts with the above
```

The rationale: `unreachable_pub` catches real API-design issues (items
accidentally exposed wider than needed).  `redundant_pub_crate` is purely
cosmetic — `pub(crate)` inside a private module is semantically correct
even if technically redundant.

---

## 7  clippy.toml — Thresholds and Toggles

The `clippy.toml` file tunes numeric thresholds and enables structural
checks that `Cargo.toml` cannot express:

| Setting | Value | Why |
|---------|-------|-----|
| `cognitive-complexity-threshold` | 30 | Default 25 is too aggressive for NTFS parser dispatch functions |
| `too-many-lines-threshold` | 150 | Default 100 breaks on well-structured pipeline functions |
| `type-complexity-threshold` | 300 | Polars lazy expressions produce deeply nested generic types |
| `enum-variant-size-threshold` | 256 | Protocol/event enums carry payloads slightly over the 200-byte default |
| `min-ident-chars-threshold` | 1 | Single-char loop vars like `i` are acceptable |
| `avoid-breaking-exported-api` | false | We are not a published library — internal refactoring should not be blocked |
| `check-private-items` | false | Would trigger 70+ `missing_errors_doc` on private helpers with self-evident error types |
| `suppress-restriction-lint-in-const` | true | Panics in `const` evaluation are compile-time errors, not runtime risks |
| `check-inconsistent-struct-field-initializers` | true | Struct initializer field order must match the definition |

---

## 8  The Two-Tier CI Strategy

The justfile defines two lint profiles:

```bash
# Production code — strictest
prod_flags := "... -W clippy::unwrap_used -W clippy::expect_used -W clippy::missing_docs_in_private_items"

# Test code — relaxed
test_flags := "... -A clippy::unwrap_used -A clippy::expect_used"
```

CI runs both:

```bash
just go    # runs lint-prod + lint-tests + test + coverage
```

This ensures that `unwrap()` in production code is always caught, even
if someone accidentally removes the `deny` from `Cargo.toml`.

---

## 9  Adding a New Suppression — The Checklist

When you need to suppress a lint, follow this process:

1. **Use `#[expect]`, not `#[allow]`** — so it warns when stale
2. **Add a `reason = "..."`** — explain *why*, not *what*
3. **Scope as tightly as possible** — prefer per-expression over per-function over per-module
4. **Never use `#![allow]` at crate level** — use `#[expect]` at the narrowest scope

```rust
// ✗ Too broad — suppresses the lint for the entire function
#[expect(clippy::indexing_slicing, reason = "...")]
fn parse_record(data: &[u8]) -> Record { ... }

// ✓ Narrow — only the specific expression is suppressed
fn parse_record(data: &[u8]) -> Record {
    #[expect(
        clippy::indexing_slicing,
        reason = "offset validated by bounds check on line 42"
    )]
    let header = &data[..HEADER_SIZE];
    ...
}
```

---

## 10  Verification

The full workspace must pass clean with zero errors, zero warnings:

```bash
# Production code (lib + bins)
cargo clippy --workspace --lib --bins --no-deps -- -D warnings

# All targets (includes tests, benches, examples)
cargo clippy --workspace --all-targets --no-deps -- -D warnings

# Tests must also pass
cargo nextest run --workspace
```

As of 2026-04-12: **13 crates, ~80 restriction lints, 1140 tests — zero errors.**
