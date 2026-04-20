# Healing log — 2026-04-19 18:56 PT

## Context

Session state before `just ship -v`:

* **10 unpushed local commits** landed during the earlier working
  session (most recent → oldest):

  ```
  0ab4b99a3 chore(toolchain): unfreeze nightly pin + wire `ship --fresh` auto-refresh
  fde351cb6 fix(daemon): self-terminate when every MFT parse fails (zombie-Ready bug)
  f4cfca4fb test(search): pin `FieldId::Bulkiness` hot-path integration end-to-end
  eca02df38 docs(changelog): catch up from v0.2.208 → v0.5.50 with milestone entries
  b3cad3137 perf(search): eliminate DisplayRow allocation on bulkiness sort-key path
  3f900859e test(client): pin async UffsClient wire-protocol + Run 10 Part B invariants
  47b463d46 docs(daemon): correct two miswritten cognitive_complexity reason strings
  b2adade0a feat(output): Phase 3.3 Windows `WriteConsoleW` direct path
  5c496aea8 test(shmem): serialise GC and concurrent-write tests via file-local mutex
  0143af245 feat(output): Phase 3.2 single-buffer multi-column console render
  234e0c165 feat(output): NUL fast path — skip row materialisation + IPC for `> NUL`
  ```

* **Rust toolchain just bumped** `nightly-2026-04-11` → `nightly-2026-04-17`
  (rustc 1.97.0-nightly `7af3402cd 2026-04-16`).  First CI run after a
  toolchain change — cold cache expected.

* **User-initiated repo changes** outside the commits above:
  - Deleted `crates/uffs-polars/Cargo.toml` (user removed the facade
    crate’s manifest during this session).
  - Bumped root `Cargo.toml` `mimalloc` dep `0.1.48` → `0.1.49`.

  Both of these are **uncommitted working-tree modifications** that
  `just ship -v` will see.  They need to be reconciled before / during
  the pipeline.

## Local advisory baselines (pre-pipeline)

Run immediately before invoking `just ship -v`:

| Check | Result |
|---|---|
| `rustc --version` | 1.97.0-nightly (`7af3402cd 2026-04-16`) |
| `cargo clippy --workspace --all-targets` | 0 errors, 0 warnings |
| `cargo clippy --workspace --no-default-features --all-targets` | 0 errors (9 pre-existing feature-gate warnings) |
| `cargo test --workspace` | 1208 passed, 0 failed |
| `bash scripts/ci/check_file_size_policy.sh` | OK |

These are **advisory** per the operating rules — the CI pipeline
verdict decides acceptance.

## Operating rules for this run

Per the user's instructions:

1. **No suppression hacks** — no blanket `#[allow]`, no disabled
   lints, no commented-out tests.  Targeted allows only when
   technically necessary, scoped + justified in-line.
2. **Surgical, correct fixes** — address root cause, not symptoms.
3. **Preserve behaviour & contracts** — public API + observable
   behaviour unchanged unless CI proves them wrong.
4. **Improve tests, don't dodge them** — strengthen coverage; never
   skip or relax.
5. **Atomic, well-described commits** — one issue per commit, clear
   `fix: <root cause>` subjects.

*Baseline and final validation must use `just ship -v`.*  Between
runs, local `cargo clippy / test / check / build` are advisory only.

## Run log

*(Populated as each pipeline iteration completes.)*

### Run 0 — preparation (this file + uncommitted cleanups)

**Goal:** stage the working tree so Run 1 starts from a known baseline.

Expected actions before Run 1:
* `uffs-polars/Cargo.toml` removal — inspect whether anything still
  references the facade crate; either restore it or remove the
  workspace member + all `uffs-polars` deps.
* `mimalloc = "0.1.49"` bump — simple lockfile refresh; should not
  break anything (patch-level bump).

---

### Run 1 — user-initiated `just ship -v` (stuck)

The user had already run the pipeline before this healing session
resumed.  Workflow state captured at hand-off:

```json
{
  "current_version": "0.5.51",
  "phase": "Clean",
  "started_at": "2026-04-20T01:37:13Z",   // 2026-04-19 18:37 PT
  "failure_count": 2,
  "last_error": "Step '09-deploy-binary' failed: Step 'Cross-platform build' failed: command 'rust-script' exited with code 1 after 29s",
  "step_tracker": {
    "completed_steps": ["00…08"],          // all of these green
    "failed_steps": ["09-deploy-binary"]
  },
  "step_durations_secs": {
    "04-coverage-tests":     90,
    "05-parallel-validation": 144,
    "08-build-release":      513,           // 8m34s
    "09-deploy-binary":       29            // fails in ~29s
  }
}
```

Observations:

* Phase 1 validation (steps 00–07) is **entirely green**.  Coverage
  tests, parallel validation (clippy + doc tests + deny), format
  check all passed — proving the local `cargo clippy/test` advisory
  baseline from the top of this log matches pipeline reality.
* Step 08 (release build) succeeded — the workspace compiles
  `--release` on nightly-2026-04-17.
* Step 09 (**deploy-binary**) fails fast: the sub-step labelled
  "Cross-platform build" calls `rust-script scripts/ci/build-cross-all.rs`
  and dies with exit code 1 after 29s.  29s ≪ a cross-compile, so it's
  failing in setup/prerequisite logic, not during actual compilation.

Next action: reproduce the failure in isolation (verbose) to see the
specific rust-script exit-1 reason, then surgically fix.  The
workflow-state file is **preserved** — a successful re-run of
`just ship -v` will resume at step 09 and promote the pipeline
through phase 2 to completion.

Additionally — noticed a stale cargo process from a previous run:

```
PID 51703  elapsed 02:23:36  cargo test --workspace --no-default-features
                             (nightly-2026-04-11 — the pre-unfreeze toolchain)
```

Orphaned, wasting CPU + holding target-dir locks.  Will confirm its
origin before taking action; it was spawned before today's
`0ab4b99a3` toolchain-unfreeze commit and is unrelated to the
current stuck deploy step.

---

### Run 2 — `just ship -v` (resumed after the stdout_kind.rs fix) ✅

**Result:** **GREEN**.  v0.5.51 shipped + pushed in auto-commit
`6a035e42d`.

#### Root cause of the Run-1 stuck state

When Run 1 reached step 09 "Cross-platform build", cargo-xwin
compiled the workspace for `x86_64-pc-windows-msvc` and died in 29 s
with two compile errors in `crates/uffs-client/src/stdout_kind.rs`:

```
error[E0061]: this function takes 4 arguments but 5 arguments were supplied
   --> crates/uffs-client/src/stdout_kind.rs:362:17
    |
362 |                 WriteConsoleW(
    |                 ^^^^^^^^^^^^^
…
364 |                     chunk.as_ptr().cast(),
    |                     --------------------- unexpected argument #3 of type `u32`

error[E0308]: mismatched types
   --> crates/uffs-client/src/stdout_kind.rs:366:26
    |
366 |                     Some(&raw mut written),
    |                          ^^^^^^^^^^^^^^^^
    |                          expected `*const c_void`, found `*mut u32`
```

Both errors come from the same upstream change in the `windows` crate
(0.62.2, current pin): `WriteConsoleW` was re-idiomatised to take a
`&[u16]` slice (which encodes both the pointer and length) instead of
the old `lpbuffer: *const u16 + nnumberofcharstowrite: u32` pair.

The Phase-3.3 commit (`b2adade0a feat(output): Phase 3.3 Windows
WriteConsoleW direct path`) was written against the older 5-arg
signature, compiled cleanly on macOS (where the `#[cfg(windows)]`
block is elided), and slipped through all macOS-host advisory gates
— `cargo clippy`, `cargo test`, and even the pipeline's Phase 1
validation, because Phase 1 only builds for the host triple.
Phase 2's step 09 (`cargo xwin build --release --target
x86_64-pc-windows-msvc`) is the first gate that actually builds the
`#[cfg(windows)]` code paths, so it caught what every earlier gate
could not.

With 5 positional args passed to a 4-arg function, rustc's arity
diagnostic pinned the extra arg on `count` (error E0061), and the
type-mismatch diagnostic pinned `Some(&raw mut written)` (a
`*mut u32`) against arg slot #4, which under the new signature is
`lpreserved: Option<*const c_void>` — hence the misleading "expected
*const c_void" note.  In reality, `Some(&raw mut written)` is correct
for `lpnumberofcharswritten: Option<*mut u32>` once the arity is
right.

#### Fix — single file, 13 lines net

`crates/uffs-client/src/stdout_kind.rs:346-365` — collapse the call
to the slice-based form:

```rust
for chunk in utf16.chunks(WRITE_CONSOLE_CHUNK_CHARS) {
    let mut written: u32 = 0;
    let result = unsafe {
        WriteConsoleW(handle, chunk, Some(&raw mut written), None)
    };
    result.map_err(std::io::Error::other)?;
    if written == 0_u32 { /* unchanged WriteZero branch */ }
}
```

Changes:

1. Passed `chunk: &[u16]` directly (was `chunk.as_ptr().cast()`).
2. Dropped the separate `count: u32` binding and its now-pointless
   `#[expect(clippy::cast_possible_truncation)]` attribute — the
   `windows` binding computes the length from the slice.
3. Tightened the SAFETY comment to describe the slice lifetime
   rather than a raw pointer+length pair.

No public API change.  Behaviour preserved: still chunk-writes up to
`WRITE_CONSOLE_CHUNK_CHARS` `u16`s per syscall, still returns
`WriteZero` on zero-progress, still propagates HRESULT errors via
`std::io::Error::other`.

**Per the operating rules**: no `#[allow]` added, no test relaxed,
no cfg gate flipped to hide the bug.  The fix is a 4→4 arg call-site
correction that matches the current upstream API.

#### Pipeline result (after the fix)

```
✅ PHASE 2 COMPLETE - Build and deploy successful!
   Version:    0.5.51
   Total time: 537s
   Steps:      12/11 completed

   Step Timings:
     00-toolchain-ensure 0s
     01-update-polars-git 0s
     02-clean-artifacts 3s
     03-format-code 0s
     04-coverage-tests 1m 30s
     05-parallel-validation 2m 24s
     06-format-check 1s
     07-version-increment 0s
     08-build-release 8m 33s
     09-deploy-binary 8m 52s     ← was the stuck step, now green
     10-git-commit 0s
     11-git-push 2s

📦 Binary uploaded to GitHub Release
📤 Changes committed and pushed
```

Auto-commit `6a035e42d` published v0.5.51:
- `crates/uffs-client/src/stdout_kind.rs` (the fix)
- `Cargo.toml` (0.5.50 → 0.5.51 + mimalloc 0.1.48 → 0.1.49)
- `CHANGELOG.md` (0.5.51 section)
- `crates/uffs-polars/Cargo.toml` (polars commit bump via
  `01-update-polars-git`)
- Pipeline-applied rustfmt reflows on 6 files (idempotent,
  behaviour-neutral).

## Summary table

| Attempt | Command        | Outcome  | Notes |
|---------|----------------|----------|-------|
| 1       | `just ship -v` | ❌ fail  | `09-deploy-binary` → Cross-platform build: 2 compile errors in `stdout_kind.rs` against `windows-0.62.2`'s new `WriteConsoleW` signature (5 args → 4, slice-based `lpbuffer`).  macOS host didn't trip on it — first real Windows-target compile happens in step 09. |
| 2       | `just ship -v` | ✅ GREEN | v0.5.51 shipped (`6a035e42d`).  Resumed at step 09 after 10-line fix; everything else skipped via completed-step cache. |

## Follow-up

This healing log itself was the first artefact that exercised the
"healing logs must be part of the commit" rule.  `.gitignore` listed
`/LOG/` as wholly-local, so the auto-commit skipped it.  Follow-up
commit after the pipeline carries:
  * `.gitignore` — negate `/LOG/*CHANGELOG_HEALING.md` so future
    healing logs participate in auto-commits by default.
  * `LOG/2026_04_19_18_56_CHANGELOG_HEALING.md` (this file).
