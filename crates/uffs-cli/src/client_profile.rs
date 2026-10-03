// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! Client-side `--profile` / `--benchmark` timing block.
//!
//! Split from `main.rs` to keep that file under the workspace 800-LOC
//! ceiling.  Purely presentational: it owns the `ClientProfile` bundle
//! and renders it to stderr, with no I/O or daemon knowledge of its
//! own.

/// What producing the output cost on the client, measured around the
/// full formatting + write pass.
#[derive(Debug, Clone, Copy)]
pub(crate) struct OutputCost {
    /// Wall-clock milliseconds for the whole output pass.
    pub(crate) ms: u128,
    /// Bytes the pass produced.  Exact for the benchmark sink (it
    /// counts every byte); for a real stdout the count is what the
    /// payload carried, not what the terminal consumed.
    pub(crate) bytes: u64,
    /// Where the bytes went: `"sink"` under `--benchmark` (formatted and
    /// discarded, so only the terminal is excluded), `"stdout"` otherwise.
    pub(crate) target: &'static str,
}

/// Which transport the daemon picked for the payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PayloadKind {
    /// No rows (no match, `--no-output`, or `--out` written by the daemon).
    Empty,
    /// Typed rows inline in the JSON envelope.
    InlineRows,
    /// Typed rows in a shared-memory file.
    ShmemRows,
    /// Pre-rendered text inline in the envelope.
    InlineBlob,
    /// Pre-rendered bytes in a shared-memory file.
    ShmemBlob,
}

/// What the profile needs to know about the payload, captured **before**
/// the output pass consumes it — the profile prints after the output so
/// it can report the output cost, and cloning a 40 M-row payload to keep
/// it around would be its own benchmark.
#[derive(Debug, Clone, Copy)]
pub(crate) struct PayloadSummary {
    /// Transport variant.
    pub(crate) kind: PayloadKind,
    /// Row count from the cheapest authoritative source for the variant
    /// (see [`Self::of`]).
    pub(crate) row_count: usize,
}

impl PayloadSummary {
    /// Summarise `payload`, resolving the row count from the cheapest
    /// authoritative source per variant:
    ///
    /// 1. `ShmemBlob` → mmap'd file; counting newlines would read every page
    ///    just to discard the count, so use the daemon's pre-computed
    ///    `total_count` instead.
    /// 2. `InlineBlob` → inline string already in memory; scanning for `\n` is
    ///    ~5 GB/s, cheap.
    /// 3. Rows variants (`InlineRows`, `ShmemRows`) → `row_count_hint()` is
    ///    O(1) — `Vec::len` or the daemon's pre-computed count.
    /// 4. `Empty` → zero rows, nothing to count.
    pub(crate) fn of(
        payload: &uffs_client::protocol::response::SearchPayload,
        total_count: u64,
    ) -> Self {
        use uffs_client::protocol::response::SearchPayload;
        match payload {
            SearchPayload::ShmemBlob(_) => Self {
                kind: PayloadKind::ShmemBlob,
                // `try_from` instead of `as` to preserve correctness on
                // hypothetical 32-bit targets where `u64` would truncate
                // (clippy::cast_possible_truncation).  `usize::MAX` is a
                // strictly larger fallback than any realistic row count.
                row_count: usize::try_from(total_count).unwrap_or(usize::MAX),
            },
            SearchPayload::InlineBlob(blob) => Self {
                kind: PayloadKind::InlineBlob,
                row_count: blob.bytes().filter(|byte| *byte == b'\n').count(),
            },
            SearchPayload::InlineRows(_) => Self {
                kind: PayloadKind::InlineRows,
                row_count: payload.row_count_hint().unwrap_or(0),
            },
            SearchPayload::ShmemRows { .. } => Self {
                kind: PayloadKind::ShmemRows,
                row_count: payload.row_count_hint().unwrap_or(0),
            },
            SearchPayload::Empty => Self {
                kind: PayloadKind::Empty,
                row_count: 0,
            },
        }
    }
}

/// Packaging these into a struct keeps `run_search` under the
/// `clippy::too-many-lines` cap and lets the profile helper take one
/// argument instead of six.
pub(crate) struct ClientProfile<'a> {
    /// Wall-clock time spent in `UffsClientSync::connect_with_args`.
    pub(crate) connect_ms: u128,
    /// Wall-clock time spent in `await_ready` (daemon warm-up).
    pub(crate) ready_ms: u128,
    /// Wall-clock time spent in the `search_cli` IPC round-trip.
    pub(crate) ipc_ms: u128,
    /// Daemon-reported SCAN duration (from the response envelope) —
    /// excludes index warm-up, reported separately below.
    pub(crate) duration_ms: u64,
    /// Daemon-reported milliseconds spent paging parked/cold drives
    /// back in before the scan.  `0` on a warm index; tens of seconds
    /// on a cold one, where it is the entire wall-clock story.
    pub(crate) promotion_ms: u64,
    /// Payload delivery channel the daemon picked for this response
    /// plus its row count, captured before the output pass consumed
    /// the payload.  Used by [`print_client_profile`] to show the
    /// transport name and the count.
    pub(crate) payload: PayloadSummary,
    /// Daemon-side `profile` object from the response envelope.  When
    /// populated, its `scan_ms` / `sort_ms` / `path_resolve_ms` /
    /// `write_ms` fields are rendered as a sub-phase breakdown inside
    /// the daemon block so the `--profile` output pinpoints where the
    /// per-query cost sits (scan vs sort vs path resolution vs disk
    /// write).
    pub(crate) daemon_profile: Option<&'a uffs_client::protocol::response::SearchProfile>,
    /// Client-side output cost, when an output pass ran (`None` under
    /// `--no-output`, where neither the daemon nor the client produces
    /// rows).
    pub(crate) output: Option<OutputCost>,
}

/// Print the `--profile` / `--benchmark` client-side timing block to
/// stderr (matches the daemon-side profile formatting).
#[expect(
    clippy::print_stderr,
    reason = "intentional --profile output to stderr"
)]
pub(crate) fn print_client_profile(prof: &ClientProfile<'_>) {
    eprintln!("=== PROFILE: Client → Daemon ===");
    eprintln!("  Connect:         {:>6} ms", prof.connect_ms);
    eprintln!("  Await ready:     {:>6} ms", prof.ready_ms);
    eprintln!(
        "  Search (IPC):    {:>6} ms  (daemon: {} ms)",
        prof.ipc_ms, prof.duration_ms
    );
    // Printed only when it happened: a warm index promotes nothing, and
    // a zero line every run would train the eye to skip it.
    if let Some(output) = prof.output {
        eprintln!(
            "  Output ({:<6}): {:>6} ms  ({} bytes formatted)",
            output.target, output.ms, output.bytes
        );
    }
    if prof.promotion_ms > 0 {
        eprintln!(
            "  Index warm-up:   {:>6} ms  (paged parked/cold drives back in)",
            prof.promotion_ms
        );
    }
    // Sub-phase breakdown from the daemon profile.  Any non-zero
    // component is printed; all-zero (regex/trigram paths, legacy
    // daemons) collapses to a single-line total.
    if let Some(dp) = prof.daemon_profile {
        let scan = dp.scan_ms;
        let sort = dp.sort_ms;
        let resolve = dp.path_resolve_ms;
        let write = dp.write_ms;
        if (scan | sort | resolve | write) > 0 {
            eprintln!(
                "    scan={scan} ms  sort={sort} ms  path_resolve={resolve} ms  write={write} ms"
            );
        }
        // Deep-profile breakdown: only present when the numeric-sort
        // branch populated the `path_*` sub-counters.  Prints per-
        // record averages derived from ns totals so the user can see
        // immediately whether the bottleneck is path-walking or
        // row-building, and whether the DirCache hit rate is high
        // enough to warrant a locality optimisation.
        let candidates = dp.path_candidates;
        let cache_entries = dp.path_cache_entries;
        let resolve_ns = dp.path_resolve_fn_ns;
        let build_ns = dp.path_build_row_ns;
        if candidates > 0 {
            let hits = candidates.saturating_sub(cache_entries);
            // Integer-math hit rate in permille (0–1000) to avoid
            // float arithmetic — clippy::float_arithmetic is banned
            // in production lints.  `permille / 10 . permille % 10`
            // prints as "99.7" for 997.
            let hit_permille = hits.saturating_mul(1000) / candidates;
            let hit_whole = hit_permille / 10;
            let hit_frac = hit_permille % 10;
            let avg_resolve_ns = resolve_ns / candidates;
            let avg_build_ns = build_ns / candidates;
            eprintln!(
                "    deep: candidates={candidates}  unique_parents={cache_entries}  \
                 hit_rate={hit_whole}.{hit_frac}%"
            );
            eprintln!(
                "          resolve_fn={} ms ({} ns/rec)  build_row={} ms ({} ns/rec)",
                resolve_ns / 1_000_000,
                avg_resolve_ns,
                build_ns / 1_000_000,
                avg_build_ns,
            );
        }
    }
    // Row count resolution lives in `PayloadSummary::of` (captured before
    // the output pass consumed the payload).
    let row_count = prof.payload.row_count;
    // Label the count by what it actually measures per transport: blob
    // variants carry rendered text (newline count includes header/footer
    // lines) or the daemon's pre-limit total, NOT the post-`--limit` page
    // (2026-06-12 dry run: `--limit 5` printed "Rows returned: 7").
    match prof.payload.kind {
        PayloadKind::ShmemBlob => {
            eprintln!("  Total matches:   {row_count:>6}");
        }
        PayloadKind::InlineBlob => {
            eprintln!("  Output lines:    {row_count:>6}");
        }
        PayloadKind::InlineRows | PayloadKind::ShmemRows | PayloadKind::Empty => {
            eprintln!("  Rows returned:   {row_count:>6}");
        }
    }
    match prof.payload.kind {
        PayloadKind::ShmemBlob => {
            eprintln!("  Transport:       shmem_blob (mmap + write_all, binary)");
        }
        PayloadKind::InlineBlob => {
            eprintln!("  Transport:       inline_blob (single write_all)");
        }
        PayloadKind::ShmemRows => {
            eprintln!("  Transport:       shmem_rows (mmap + per-row format)");
        }
        PayloadKind::InlineRows | PayloadKind::Empty => {
            // inline_rows is the default — no extra line needed.
            // empty responses skip the transport line entirely.
        }
    }
}
