# UFFS Crate Scaffolding & Reorganization Guide

> **Status**: Active  
> **Date**: 2026-03-26  
> **Scope**: New crate creation, dependency cleanup, re-export hygiene  
> **References**: `DAEMON_SERVICE_ARCHITECTURE.md`, `SECURITY_IMPLEMENTATION_PLAN.md`

---

## Executive Summary

This document is the step-by-step guide for reorganizing the UFFS workspace
from 7 crates to 12 crates, cleaning up dependency hand-me-downs, and
establishing the foundation for the daemon architecture and security features.

### Target Crate Layout

```
crates/
  ── Existing (unchanged) ──────────────────────────
  uffs-polars/     # Polars facade (compilation isolation)
  uffs-core/       # Query engine (search, pattern, path resolution)
  uffs-diag/       # Diagnostic tools

  ── Existing (modified) ───────────────────────────
  uffs-mft/        # MFT reading + cache (uses uffs-security for encryption)
  uffs-cli/        # CLI binary — later adopts uffs-client as dep (daemon Phase 3)
  uffs-tui/        # TUI binary — later adopts uffs-client as dep (daemon Phase 4)
  uffs-gui/        # GUI binary — adopts uffs-client as dep from start

  ── New: Security ─────────────────────────────────
  uffs-security/   # Crypto, key storage, secure FS ops, TLS

  ── New: Daemon Architecture ──────────────────────
  uffs-daemon/     # Background service process
  uffs-client/     # Thin client library (all surfaces)
  uffs-mcp/        # MCP stdio adapter
  uffs-broker/     # Windows elevated handle broker (optional)
```

### Target Dependency Graph

```
                SURFACES (binary crates)
  ┌──────┐  ┌──────┐  ┌──────┐  ┌──────┐
  │ CLI  │  │ TUI  │  │ GUI  │  │ MCP  │
  └──┬───┘  └──┬───┘  └──┬───┘  └──┬───┘
     │         │         │          │
     └─────────┴─────────┴──────────┘
                    │
             ┌──────┴──────┐
             │ uffs-client  │──► uffs-security
             └──────┬──────┘
                    │ IPC
             ┌──────┴──────┐
             │ uffs-daemon  │──► uffs-security
             └──┬───┬──────┘
                │   │
                ▼   ▼
          uffs-core  uffs-mft──► uffs-security
               │         │
               ▼         ▼
          uffs-mft   uffs-polars
               │
               ▼
          uffs-polars

  uffs-broker──► uffs-security  (Windows only)
```

---

## Current State: Dependency Audit

### Problem 1: Polars Re-Export Chain (hand-me-down)

`uffs-mft` re-exports Polars types from `uffs-polars`:

```rust
// uffs-mft/src/lib.rs line 184
pub use uffs_polars::{DataFrame, IntoLazy, LazyFrame, col, lit};
```

Then consumers use these via `uffs_mft::`:

```rust
// uffs-cli uses Polars types through uffs-mft re-exports
use uffs_mft::DataFrame;    // hand-me-down from uffs-polars
use uffs_mft::LazyFrame;    // hand-me-down
use uffs_mft::col;           // hand-me-down
```

**33 call sites** in `uffs-cli` use `uffs_mft::DataFrame/LazyFrame/col/lit`.

**Fix**: These should import directly from `uffs_polars::` since `uffs-cli`
already has a direct `uffs-polars` dependency.

### Problem 2: Formatter Functions in Wrong Crate

`uffs-mft` contains display formatters that are used by both CLI and TUI:

```rust
// uffs-mft/src/lib.rs lines 199-340
pub fn format_number_commas(num: u64) -> String { ... }
pub fn format_bytes(bytes: u64) -> String { ... }
pub fn format_timestamp(unix_micros: i64) -> String { ... }
pub fn format_bool(value: bool) -> &'static str { ... }
pub fn format_duration(duration: Duration) -> String { ... }
```

These are **display utilities**, not MFT reading logic. They belong in a
shared utility location.

**14 call sites** in `uffs-tui` use `uffs_mft::format_*`.
**Multiple call sites** in `uffs-cli` use them indirectly.

**Fix**: Move to `uffs-core` (which is the shared library crate) or to a
tiny `uffs-common` module re-exported from `uffs-core`.

### Problem 3: TUI Direct Dependency on `uffs-mft` Internals

`uffs-tui` imports directly from `uffs-mft`:

```rust
use uffs_mft::cache::cache_file_path;   // cache location
use uffs_mft::index::MftIndex;          // full index type
use uffs_mft::format_*;                 // display formatters
```

When the TUI migrates to `uffs-client`, it should NOT depend on `uffs-mft`
at all. The daemon owns the index; the TUI only sees search results.

### Problem 4: GUI Placeholder Has Heavy Dependencies

`uffs-gui` currently depends on `uffs-polars`, `uffs-mft`, `uffs-core` — all
of which it doesn't use (it's a placeholder). When it becomes a real GUI, it
should only depend on `uffs-client`.

---

## Implementation Phases

### Phase C1: Create `uffs-security` Crate (Scaffold)

> **Effort**: 1 hour  
> **Dependencies**: None  
> **Blocking**: Security implementation (S1-S3)

#### Tasks

| ID | Task | Status |
|----|------|--------|
| C1.1 | Create `crates/uffs-security/Cargo.toml` with minimal deps | ⬜ TODO |
| C1.2 | Create `crates/uffs-security/src/lib.rs` with module stubs | ⬜ TODO |
| C1.3 | Create stub files: `crypto.rs`, `keystore.rs`, `fs.rs` | ⬜ TODO |
| C1.4 | Add `uffs-security` to workspace `Cargo.toml` members + deps | ⬜ TODO |
| C1.5 | Add `uffs-security` dependency to `uffs-mft` | ⬜ TODO |
| C1.6 | Verify: `cargo check --workspace` passes | ⬜ TODO |

#### `crates/uffs-security/Cargo.toml`

```toml
[package]
name = "uffs-security"
description = "Security primitives for UFFS: encryption, key storage, secure FS ops"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true
authors.workspace = true

[dependencies]
# Crypto (added when implementing Phase S2)
# aes-gcm = "0.10"
# rand = "0.8"

# Logging
tracing.workspace = true

# Platform dirs (for key storage paths)
dirs-next.workspace = true

# Platform key storage — added when implementing Phase S2
# [target.'cfg(target_os = "macos")'.dependencies]
# security-framework = "2.11"
#
# [target.'cfg(target_os = "linux")'.dependencies]
# secret-service = "4.0"

[lints]
workspace = true
```

#### `crates/uffs-security/src/lib.rs`

```rust
//! Security primitives for UFFS.
//!
//! This crate provides encryption, key management, and secure filesystem
//! operations. It has **no dependency** on MFT, search, or UI crates.
//!
//! # Modules
//!
//! - [`crypto`] — AES-256-GCM authenticated encryption
//! - [`keystore`] — Platform-native key storage (DPAPI / Keychain / Secret Service)
//! - [`fs`] — Secure file operations (atomic write, secure delete, permissions)

pub mod crypto;
pub mod fs;
pub mod keystore;
```

#### Stub modules

Each module starts with a doc comment and placeholder types/functions
that will be filled in during security implementation phases:

```rust
// crypto.rs
//! AES-256-GCM authenticated encryption for cache files.

// keystore.rs
//! Platform-native secure key storage.

// fs.rs
//! Secure filesystem operations: atomic writes, secure delete, permissions.
```

---

### Phase C2: Create `uffs-client` Crate (Scaffold)

> **Effort**: 1 hour  
> **Dependencies**: None  
> **Blocking**: Daemon Phase 1

#### Tasks

| ID | Task | Status |
|----|------|--------|
| C2.1 | Create `crates/uffs-client/Cargo.toml` | ⬜ TODO |
| C2.2 | Create `crates/uffs-client/src/lib.rs` with placeholder API | ⬜ TODO |
| C2.3 | Add to workspace members + deps | ⬜ TODO |
| C2.4 | Verify: `cargo check --workspace` passes | ⬜ TODO |

#### `crates/uffs-client/Cargo.toml`

```toml
[package]
name = "uffs-client"
description = "Thin client library for UFFS daemon — connect, query, lifecycle"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true
authors.workspace = true

[dependencies]
# Internal
uffs-security = { path = "../uffs-security" }

# Async
tokio.workspace = true

# Serialization (JSON-RPC)
serde.workspace = true
serde_json.workspace = true

# Error handling
thiserror.workspace = true

# Logging
tracing.workspace = true

[lints]
workspace = true
```

#### `crates/uffs-client/src/lib.rs`

```rust
//! Thin client library for the UFFS daemon.
//!
//! All surfaces (CLI, TUI, GUI, MCP) use this crate to communicate with
//! the daemon. It handles auto-start, connection, keepalive, and reconnect.
//!
//! # Example
//!
//! ```rust,ignore
//! let client = UffsClient::connect().await?;
//! let results = client.search("*.rs").await?;
//! let drives = client.drives().await?;
//! ```

pub mod connect;
pub mod error;
pub mod query;
pub mod types;
```

---

### Phase C3: Create `uffs-daemon` Crate (Scaffold)

> **Effort**: 1 hour  
> **Dependencies**: C1 (uffs-security), C2 (uffs-client for types)  
> **Blocking**: Daemon Phase 1

#### Tasks

| ID | Task | Status |
|----|------|--------|
| C3.1 | Create `crates/uffs-daemon/Cargo.toml` | ⬜ TODO |
| C3.2 | Create `crates/uffs-daemon/src/main.rs` with minimal daemon scaffold | ⬜ TODO |
| C3.3 | Create stub modules: `ipc.rs`, `handler.rs`, `lifecycle.rs`, `index.rs` | ⬜ TODO |
| C3.4 | Add to workspace members + deps | ⬜ TODO |
| C3.5 | Verify: `cargo check --workspace` passes | ⬜ TODO |

#### `crates/uffs-daemon/Cargo.toml`

```toml
[package]
name = "uffs-daemon"
description = "UFFS background daemon — holds MFT index, serves queries via IPC"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true
authors.workspace = true

[[bin]]
name = "uffs-daemon"
path = "src/main.rs"

[dependencies]
# Internal
uffs-security = { path = "../uffs-security" }
uffs-mft.workspace = true
uffs-core.workspace = true

# Async
tokio.workspace = true

# Serialization (JSON-RPC)
serde.workspace = true
serde_json.workspace = true

# Error handling
thiserror.workspace = true
anyhow.workspace = true

# Logging
tracing.workspace = true
tracing-subscriber.workspace = true
tracing-appender.workspace = true

# Platform dirs
dirs-next.workspace = true

[lints]
workspace = true
```

---

### Phase C4: Create `uffs-mcp` Crate (Scaffold)

> **Effort**: 30 min  
> **Dependencies**: C2 (uffs-client)  
> **Blocking**: Daemon Phase 2

#### Tasks

| ID | Task | Status |
|----|------|--------|
| C4.1 | Create `crates/uffs-mcp/Cargo.toml` | ⬜ TODO |
| C4.2 | Create `crates/uffs-mcp/src/main.rs` with placeholder | ⬜ TODO |
| C4.3 | Add to workspace members | ⬜ TODO |
| C4.4 | Verify: `cargo check --workspace` passes | ⬜ TODO |

#### `crates/uffs-mcp/Cargo.toml`

```toml
[package]
name = "uffs-mcp"
description = "MCP stdio adapter for UFFS — bridges AI agents to the daemon"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true
authors.workspace = true

[[bin]]
name = "uffs-mcp"
path = "src/main.rs"

[dependencies]
# Internal
uffs-client = { path = "../uffs-client" }

# Async
tokio.workspace = true

# Serialization (MCP JSON-RPC)
serde.workspace = true
serde_json.workspace = true

# Error handling
anyhow.workspace = true

# Logging
tracing.workspace = true

[lints]
workspace = true
```

---

### Phase C5: Create `uffs-broker` Crate (Scaffold, Windows only)

> **Effort**: 30 min  
> **Dependencies**: C1 (uffs-security)  
> **Blocking**: Daemon Phase 5

#### Tasks

| ID | Task | Status |
|----|------|--------|
| C5.1 | Create `crates/uffs-broker/Cargo.toml` | ⬜ TODO |
| C5.2 | Create `crates/uffs-broker/src/main.rs` with placeholder | ⬜ TODO |
| C5.3 | Add to workspace members | ⬜ TODO |
| C5.4 | Verify: `cargo check --workspace` passes | ⬜ TODO |

#### `crates/uffs-broker/Cargo.toml`

```toml
[package]
name = "uffs-broker"
description = "UFFS Access Broker — Windows service for elevated MFT handle brokering"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true
authors.workspace = true

[[bin]]
name = "uffs-broker"
path = "src/main.rs"

[dependencies]
# Internal
uffs-security = { path = "../uffs-security" }

# Error handling
anyhow.workspace = true

# Logging
tracing.workspace = true

[target.'cfg(windows)'.dependencies]
windows.workspace = true

[lints]
workspace = true
```

---

### Phase C6: Dependency Cleanup — Polars Re-Export Chain

> **Effort**: 2–3 hours  
> **Dependencies**: None (can be done independently)  
> **Risk**: Low — mechanical find-and-replace

The goal: consumers import Polars types from `uffs_polars` directly, not
through the `uffs_mft` hand-me-down re-export.

#### Tasks

| ID | Task | Status |
|----|------|--------|
| C6.1 | In `uffs-cli`: replace all `uffs_mft::DataFrame` → `uffs_polars::DataFrame` (33 sites) | ⬜ TODO |
| C6.2 | In `uffs-cli`: replace all `uffs_mft::LazyFrame` → `uffs_polars::LazyFrame` | ⬜ TODO |
| C6.3 | In `uffs-cli`: replace all `uffs_mft::col` → `uffs_polars::col` | ⬜ TODO |
| C6.4 | In `uffs-cli`: replace all `uffs_mft::lit` → `uffs_polars::lit` | ⬜ TODO |
| C6.5 | In `uffs-cli`: replace all `uffs_mft::IntoLazy` → `uffs_polars::IntoLazy` | ⬜ TODO |
| C6.6 | Remove Polars re-exports from `uffs-mft/src/lib.rs` line 184 | ⬜ TODO |
| C6.7 | Verify: `cargo check --workspace` passes | ⬜ TODO |
| C6.8 | Verify: `cargo test --workspace` passes | ⬜ TODO |

**Mechanical transformation** — every change is:
```rust
// Before
use uffs_mft::DataFrame;
// After
use uffs_polars::DataFrame;
```

**Note**: `uffs-mft` internally still uses `uffs_polars::*` directly (it has
the dep). The only change is removing the `pub use` re-export line in `lib.rs`
and updating downstream consumers.

---

### Phase C7: Dependency Cleanup — Format Functions

> **Effort**: 1–2 hours  
> **Dependencies**: None  
> **Risk**: Low — move + re-export for backward compat

Display formatters (`format_bytes`, `format_timestamp`, etc.) don't belong in
the MFT parsing crate. They're shared utilities.

#### Option A: Move to `uffs-core` (recommended)

`uffs-core` is already the shared library between CLI, TUI, and future daemon.
Add a `format` module there.

#### Option B: Leave in `uffs-mft` with deprecation

Less disruptive short-term. Add a `#[deprecated]` re-export pointing to
the new location.

#### Tasks (Option A)

| ID | Task | Status |
|----|------|--------|
| C7.1 | Create `crates/uffs-core/src/format.rs` with the 5 formatter functions (copy from uffs-mft) | ⬜ TODO |
| C7.2 | Export from `uffs-core/src/lib.rs`: `pub mod format;` | ⬜ TODO |
| C7.3 | In `uffs-tui`: replace `uffs_mft::format_*` → `uffs_core::format::format_*` (14 sites) | ⬜ TODO |
| C7.4 | In `uffs-mft/src/lib.rs`: remove formatter functions (lines 195-340), add deprecation re-exports pointing to `uffs_core::format::*` OR just remove if no external consumers | ⬜ TODO |
| C7.5 | Update any `uffs-cli` code that uses formatters via `uffs-mft` | ⬜ TODO |
| C7.6 | Verify: `cargo check --workspace` && `cargo test --workspace` | ⬜ TODO |

---

### Phase C8: Clean Up GUI Placeholder Dependencies

> **Effort**: 15 min  
> **Dependencies**: C2 (uffs-client exists)

#### Tasks

| ID | Task | Status |
|----|------|--------|
| C8.1 | Replace `uffs-polars`, `uffs-mft`, `uffs-core` deps with `uffs-client` in `uffs-gui/Cargo.toml` | ⬜ TODO |
| C8.2 | Update `uffs-gui/src/main.rs` placeholder to reference `uffs-client` | ⬜ TODO |
| C8.3 | Verify: `cargo check --workspace` passes | ⬜ TODO |

#### Target `uffs-gui/Cargo.toml` dependencies section

```toml
[dependencies]
# Internal — GUI is a thin client
uffs-client = { path = "../uffs-client" }

# Async runtime
tokio.workspace = true

# Error handling
anyhow.workspace = true

# Logging
tracing.workspace = true
tracing-subscriber.workspace = true

# CLI (for startup flags)
clap.workspace = true

# GUI framework (placeholder)
# egui = "0.28"
# eframe = "0.28"
```

---

### Phase C9: Workspace `Cargo.toml` — Add All New Crates

> **Effort**: 15 min  
> **Dependencies**: C1-C5 directories exist

#### Target `Cargo.toml` workspace section

```toml
[workspace]
resolver = "2"
members = [
    # ── Foundation ──
    "crates/uffs-polars",     # Polars facade (compilation isolation)
    "crates/uffs-security",   # Crypto, key storage, secure FS ops
    "crates/uffs-mft",        # MFT reading & parsing
    "crates/uffs-core",       # Query engine

    # ── Daemon Architecture ──
    "crates/uffs-daemon",     # Background service process
    "crates/uffs-client",     # Thin client library
    "crates/uffs-mcp",        # MCP stdio adapter
    "crates/uffs-broker",     # Windows access broker (optional)

    # ── Surfaces (binary crates) ──
    "crates/uffs-cli",        # Command-line interface
    "crates/uffs-tui",        # Terminal UI
    "crates/uffs-gui",        # Graphical UI

    # ── Tools ──
    "crates/uffs-diag",       # Diagnostic tools (not shipped)
]
```

#### New workspace dependency aliases

```toml
[workspace.dependencies]
# ───── Internal Crates ─────
uffs-polars   = { path = "crates/uffs-polars" }
uffs-security = { path = "crates/uffs-security" }
uffs-mft      = { path = "crates/uffs-mft", features = ["zstd"] }
uffs-core     = { path = "crates/uffs-core" }
uffs-client   = { path = "crates/uffs-client" }
```

---

## Dependency Layering Rules

These rules prevent future dependency tangles:

### Layer 0: Foundation (no internal deps)

| Crate | May Depend On |
|-------|--------------|
| `uffs-polars` | External only (polars) |
| `uffs-security` | External only (aes-gcm, rand, platform key crates) |

### Layer 1: Data (depends on Layer 0 only)

| Crate | May Depend On |
|-------|--------------|
| `uffs-mft` | `uffs-polars`, `uffs-security` |

### Layer 2: Logic (depends on Layer 0-1)

| Crate | May Depend On |
|-------|--------------|
| `uffs-core` | `uffs-mft`, `uffs-polars` |

### Layer 3: Services (depends on Layer 0-2)

| Crate | May Depend On |
|-------|--------------|
| `uffs-daemon` | `uffs-core`, `uffs-mft`, `uffs-security` |
| `uffs-client` | `uffs-security` |
| `uffs-broker` | `uffs-security` |

### Layer 4: Surfaces (depends on Layer 3 only)

| Crate | May Depend On | MUST NOT depend on |
|-------|--------------|-------------------|
| `uffs-cli` | `uffs-client` (after migration), `uffs-core`+`uffs-mft` (standalone mode) | — |
| `uffs-tui` | `uffs-client` (after migration), `uffs-core`+`uffs-mft` (current) | — |
| `uffs-gui` | `uffs-client` only | `uffs-mft`, `uffs-core`, `uffs-polars` |
| `uffs-mcp` | `uffs-client` only | everything else |

### Forbidden Dependencies (enforce in CI)

```
uffs-security  MUST NOT depend on  uffs-mft, uffs-core, uffs-polars
uffs-client    MUST NOT depend on  uffs-mft, uffs-core, uffs-polars
uffs-mcp       MUST NOT depend on  uffs-mft, uffs-core, uffs-polars
uffs-polars    MUST NOT depend on  any internal crate
uffs-broker    MUST NOT depend on  uffs-mft, uffs-core, uffs-polars
```

These can be enforced with `cargo-deny` or a simple CI script:

```bash
# CI check: forbidden deps
cargo metadata --format-version 1 | jq -r '
  .resolve.nodes[] |
  select(.id | contains("uffs-security")) |
  .deps[] | select(.name | test("uffs-(mft|core|polars)")) |
  "ERROR: uffs-security depends on \(.name)"
'
```

---

## Implementation Order

The phases can be parallelized:

```
IMMEDIATE (no blockers):
  C1  Create uffs-security scaffold ─────── blocks S1-S3 (security impl)
  C6  Polars re-export cleanup ──────────── standalone, no blockers
  C7  Format function cleanup ───────────── standalone, no blockers
  C9  Workspace Cargo.toml updates ──────── do alongside C1-C5

AFTER C1:
  C2  Create uffs-client scaffold ────────── blocks daemon Phase 1
  C3  Create uffs-daemon scaffold ────────── blocks daemon Phase 1

AFTER C2:
  C4  Create uffs-mcp scaffold ──────────── blocks daemon Phase 2
  C8  GUI dependency cleanup ────────────── quick win

DEFERRED (implement with daemon Phase 5):
  C5  Create uffs-broker scaffold
```

### Recommended Sequence (single developer)

```
Day 1 (scaffolding):
  1. C9  — Update workspace Cargo.toml (add members, deps)
  2. C1  — Create uffs-security (scaffold)
  3. C2  — Create uffs-client (scaffold)
  4. C3  — Create uffs-daemon (scaffold)
  5. C4  — Create uffs-mcp (scaffold)
  6.      cargo check --workspace (verify all green)

Day 2 (cleanup):
  7. C6  — Polars re-export cleanup (33 call sites)
  8. C7  — Format function move to uffs-core
  9. C8  — GUI dependency cleanup
  10.     cargo check --workspace && cargo test --workspace
  11.     Commit: "refactor: crate scaffolding + dependency cleanup"
```

---

## Progress Tracking

### Phase Status

| Phase | Name | Tasks | Done | Status |
|-------|------|-------|------|--------|
| C1 | uffs-security scaffold | 6 | 6 | ✅ |
| C2 | uffs-client scaffold | 4 | 0 | ⬜ |
| C3 | uffs-daemon scaffold | 5 | 0 | ⬜ |
| C4 | uffs-mcp scaffold | 4 | 0 | ⬜ |
| C5 | uffs-broker scaffold | 4 | 0 | ⬜ (deferred) |
| C6 | Polars re-export cleanup | 8 | 8 | ✅ |
| C7 | Format function cleanup | 6 | 6 | ✅ |
| C8 | GUI dep cleanup | 3 | 0 | ⬜ |
| C9 | Workspace Cargo.toml | 1 | 1 | ✅ (done with C1) |
| **TOTAL** | | **41** | **21** | |

### Completion Log

```
Date        | ID     | Description                         | Commit
────────────┼────────┼─────────────────────────────────────┼────────
2026-03-26  | C1     | uffs-security crate (scaffold+impl) | (pending)
2026-03-26  | C9     | Workspace Cargo.toml (uffs-security) | (pending)
2026-03-26  | C6     | Polars re-export cleanup (33+2 sites)| (pending)
2026-03-26  | C7     | format_* moved to uffs-core/format.rs| (pending)
```

---

## Migration Timeline (Surface Crates → uffs-client)

The surface crates (CLI, TUI, GUI) will eventually migrate from direct
`uffs-mft`/`uffs-core` deps to `uffs-client`. This happens in daemon
phases, NOT during scaffolding:

| Surface | Current Deps | After Scaffold | After Daemon Migration |
|---------|-------------|---------------|----------------------|
| **CLI** | uffs-mft, uffs-core, uffs-polars | Same (no change yet) | `uffs-client` + `--standalone` fallback to uffs-mft |
| **TUI** | uffs-mft, uffs-core, uffs-polars | Same (no change yet) | `uffs-client` only (<50 MB) |
| **GUI** | uffs-mft, uffs-core, uffs-polars | `uffs-client` (cleaned up) | `uffs-client` only |
| **MCP** | N/A (new) | `uffs-client` | `uffs-client` only |

The CLI keeps a `--standalone` mode that uses `uffs-mft` directly (no daemon)
for scripting environments. This means CLI will always have both deps — that's
fine and intentional.

---

*Document Version: 1.0*  
*Last Updated: 2026-03-26*
