# UFFS Licensing Implementation Plan

Date: 2026-04-12
Status: implementation checklist
Decision: **MPL-2.0** for the public UFFS platform, proprietary for paid products
Prior art: `UFFS_licensing_commercialization_strategy_2026-04-08.md`

---

## 0. Current State (what is broken)

| Item | Problem | Severity |
|------|---------|----------|
| `LICENSE` (root) | Says "TTAPI - Tastytrade API", "Copyright SKY, LLC.", contact `skylegal@nios.net` | 🔴 Critical |
| `REUSE.toml` | Declares **all** files as `MIT OR Apache-2.0` — contradicts actual intent | 🔴 Critical |
| `LICENSES/LicenseRef-SKY-Proprietary.txt` | "For internal SKY, LLC. use only" — repo-wide default claim | 🔴 Critical |
| `LICENSES/LicenseRef-TTAPI-Commercial.txt` | References "TTAPI", not UFFS; points to non-existent `LICENSE-COMMERCIAL` | 🟡 High |
| `LICENSES/LicenseRef-Proprietary.txt` | Legacy "pending migration" marker; references `LicenseRef-TTAPI-Commercial` | 🟡 High |
| `LICENSES/Apache-2.0.txt` | Not needed if the core is MPL-2.0 only | 🟢 Low |
| `LICENSES/MIT.txt` | Not needed if the core is MPL-2.0 only | 🟢 Low |
| `Cargo.toml` | `license = "MPL-2.0 OR LicenseRef-UFFS-Commercial"` but no such file exists | 🟡 High |
| Source `.rs` files | No SPDX headers in any crate source file | 🟡 High |
| Config `.toml` files | `.config/coverage.toml`, `.config/nextest.toml`, `rustfmt.toml` say `MIT OR Apache-2.0` | 🟡 High |
| Scripts `.rs` | Correct (`MPL-2.0`) but reference old copyright line | 🟢 Low |

---

## 1. LICENSES/ Directory — Target State

### Delete (3 files)

```
LICENSES/LicenseRef-Proprietary.txt        ← legacy migration marker, no longer needed
LICENSES/LicenseRef-SKY-Proprietary.txt    ← SKY internal-only, trust-destroying in a public repo
LICENSES/LicenseRef-TTAPI-Commercial.txt   ← wrong project name, points to nonexistent file
```

### Keep (1 file)

```
LICENSES/MPL-2.0.txt                       ← the one true license for the public platform
```

### Decide: Apache-2.0.txt and MIT.txt

Two options:

**Option A (recommended): Remove both.** The public UFFS repo is MPL-2.0. Period. No ambiguity.
Config/tooling files (`.toml`, `.json`, `.yaml`) are not copyrightable works worth a separate
license. Keeping Apache/MIT for "config files" creates exactly the confusing multi-license
story we're trying to eliminate.

**Option B: Keep for non-code files.** If you later want docs or schemas under Apache-2.0,
keep `Apache-2.0.txt`. But do this only when you actually have files under that license.

### Create (1 file, only if keeping dual-license commercial option)

If you want to preserve `MPL-2.0 OR LicenseRef-UFFS-Commercial` in Cargo.toml for future
OEM/commercial exception sales:

```
LICENSES/LicenseRef-UFFS-Commercial.txt    ← new, UFFS-branded commercial license reference
```

Contents should explain: what the commercial license grants (no copyleft obligation, private
modification rights, commercial distribution, support), how to inquire, and who the licensor is.

If you are **not** ready to sell commercial exceptions yet, simplify `Cargo.toml` to just
`license = "MPL-2.0"` and add the commercial option later when you have actual terms written.

### Result

```
LICENSES/
├── MPL-2.0.txt                          ← public platform license
└── LicenseRef-UFFS-Commercial.txt       ← (optional) commercial exception reference
```

---

## 2. Root LICENSE File

Replace the current `LICENSE` entirely. It currently says TTAPI/SKY.

### New `LICENSE` content

```
Mozilla Public License Version 2.0
==================================

This Source Code Form is subject to the terms of the Mozilla Public
License, v. 2.0. If a copy of the MPL was not distributed with this
file, You can obtain one at https://mozilla.org/MPL/2.0/.

UFFS — Ultra Fast File Search
Copyright (c) 2025-2026 SKY, LLC.

The full text of the Mozilla Public License 2.0 can be found in
LICENSES/MPL-2.0.txt and at https://mozilla.org/MPL/2.0/.
```

That's it. No dual-license noise, no commercial contact, no SKY, no TTAPI. Clean.

If you later add a commercial option, add a **separate** `COMMERCIAL.md` — do not pollute `LICENSE`.

---

## 3. REUSE.toml

Replace the current content. The current file declares everything as `MIT OR Apache-2.0`.

### New `REUSE.toml` content

```toml
version = 1

# Default: all files in the UFFS repository are MPL-2.0
[[annotations]]
path = "**"
SPDX-FileCopyrightText = "2025-2026 SKY, LLC."
SPDX-License-Identifier = "MPL-2.0"
```

---

## 4. Cargo.toml Workspace Metadata

### Current (line 51)
```toml
license = "MPL-2.0 OR LicenseRef-UFFS-Commercial"
```

### Decision

- If `LicenseRef-UFFS-Commercial.txt` exists and has real terms → keep as-is
- If not ready for commercial terms yet → simplify to:

```toml
license = "MPL-2.0"
```

Change it back to dual when the commercial license is actually written and reviewed by counsel.

---

## 5. SPDX Headers in Source Files

Every `.rs` source file in `crates/` should have this header as the first lines:

```rust
// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.
```

### Automation

Run a one-liner to add headers to all `.rs` files that don't have them:

```bash
find crates/ -name '*.rs' | while read f; do
  if ! head -1 "$f" | grep -q 'SPDX-License-Identifier'; then
    (echo '// SPDX-License-Identifier: MPL-2.0'; echo '// Copyright (c) 2025-2026 SKY, LLC.'; echo ''; cat "$f") > "$f.tmp" && mv "$f.tmp" "$f"
  fi
done
```

### Config/tooling files

Fix the incorrect `MIT OR Apache-2.0` headers in:
- `.config/coverage.toml` → `# SPDX-License-Identifier: MPL-2.0`
- `.config/nextest.toml` → `# SPDX-License-Identifier: MPL-2.0`
- `rustfmt.toml` → `# SPDX-License-Identifier: MPL-2.0`
- `deny.toml` → already correct (`MPL-2.0`)

---

## 6. README.md — Prominent License Statement

Add a clear open-source commitment block near the top of the README, right after the
badges and tagline. This is the single most important trust signal for developers,
enterprises, and contributors.

### What to add (after the badges, before "Why UFFS?")

> **Open source, forever.** The UFFS platform — engine, daemon, CLI, and MCP server — is
> licensed under the [Mozilla Public License 2.0](LICENSE). Code released as part of UFFS
> Core will never be made less open. Commercial products and enterprise offerings are built
> on top of the open platform, not by restricting it.

### What to update (bottom License section)

Replace the current minimal one-liner with:

```markdown
## License

UFFS is licensed under the [Mozilla Public License 2.0 (MPL-2.0)](LICENSE).

You can use, modify, and distribute UFFS freely. If you modify MPL-covered source files
and distribute the result, those file-level changes must remain under MPL-2.0.
Building proprietary applications on top of UFFS does not require opening your application.

See [LICENSES/MPL-2.0.txt](LICENSES/MPL-2.0.txt) for the full license text and
[Mozilla's MPL FAQ](https://www.mozilla.org/en-US/MPL/2.0/FAQ/) for plain-language guidance.
```

---

## 7. Code Separation — Public vs. Private Repos

### 7.1 What stays in this public repo

| Crate | Role | License |
|-------|------|---------|
| `uffs-polars` | Polars compilation facade | MPL-2.0 |
| `uffs-security` | Crypto, key storage | MPL-2.0 |
| `uffs-text` | Unicode text processing | MPL-2.0 |
| `uffs-mft` | MFT reading → DataFrame | MPL-2.0 |
| `uffs-core` | Query engine | MPL-2.0 |
| `uffs-daemon` | Background service | MPL-2.0 |
| `uffs-client` | Thin client library | MPL-2.0 |
| `uffs-mcp` | MCP adapter for AI agents | MPL-2.0 |
| `uffs-broker` | Windows elevated handle broker | MPL-2.0 |
| `uffs-cli` | Command-line interface | MPL-2.0 |
| `uffs-diag` | Diagnostic tools | MPL-2.0 |

### 7.2 What moves to a new private repo (`uffs-products`)

| Crate | Role | License |
|-------|------|---------|
| `uffs-tui` | Terminal UI (paid product) | Proprietary |
| `uffs-gui` | Graphical UI / UFFS Studio | Proprietary |
| `uffs-forensics` (new) | Forensic analysis packs | Proprietary |
| `uffs-enterprise` (new) | Fleet/compliance/SSO | Proprietary |

### 7.3 Dependency audit: current state of `uffs-tui`

**Problem:** `uffs-tui` currently depends directly on `uffs-core` internals, not just `uffs-client`.

Current `uffs-tui` imports from `uffs-core`:
- `uffs_core::search::backend::{DisplayRow, FilterMode, MultiDriveBackend, ...}`
- `uffs_core::search::field::FieldId`
- `uffs_core::search::filters::{SearchFilters, apply_filter, apply_search_filters}`
- `uffs_core::search::columns::{DEFAULT_COLUMNS, parse_columns}`
- `uffs_core::trigram::TrigramIndex`
- `uffs_core::compact::{DriveCompactIndex, ...}`

It also has `uffs-polars` and `uffs-mft` in its `Cargo.toml` dependencies, though those
are not directly imported in any source file (likely transitive or unused).

One import from `uffs-text`: `uffs_text::CaseFold::default_table()` in `refresh.rs`.

**What this means:** The TUI currently bypasses the daemon for some operations and uses
the core engine directly. Before moving it to the private repo, either:

- **(Option A — recommended for now):** Accept the dependency on `uffs-core` and list it as
  a git dependency in the private repo. This works fine — `uffs-core` is MPL-2.0 and a
  proprietary TUI can link against MPL libraries without issue under MPL's "Larger Work"
  provision. The TUI does not modify `uffs-core` files, so no copyleft obligation triggers.

- **(Option B — long-term):** Refactor the TUI to use only `uffs-client` for all search
  operations, making it a pure daemon client. Move shared types like `DisplayRow`, `FieldId`,
  `SearchFilters` into `uffs-client` or a new `uffs-types` crate. This is cleaner but is a
  significant refactoring effort — do it later, not as part of the initial split.

`uffs-gui` is clean — it only depends on `uffs-client`. No issues.

### 7.4 Creating the private repo — step by step

#### Step 1: Create the GitHub repo

```bash
# On GitHub: create private repo "uffs-products" under your account
# Then clone it locally next to the public repo:
cd ~/Private/Github
git clone git@github.com:githubrobbi/uffs-products.git
```

Directory layout after creation:
```
~/Private/Github/
├── UltraFastFileSearch/     ← public repo (UFFS Core)
└── uffs-products/           ← private repo (paid products)
```

#### Step 2: Create the workspace scaffold

```bash
cd uffs-products
mkdir -p crates/uffs-tui/src
mkdir -p crates/uffs-gui/src
mkdir -p crates/uffs-forensics/src
```

#### Step 3: Create `uffs-products/Cargo.toml`

```toml
# ============================================================================
# UFFS Products — Commercial products built on the UFFS open-source platform
# ============================================================================
# SPDX-License-Identifier: LicenseRef-UFFS-Proprietary
# Copyright (c) 2025-2026 SKY, LLC.
#
# This repository contains proprietary UFFS products.
# The UFFS Core platform is open source under MPL-2.0 at:
# https://github.com/githubrobbi/UltraFastFileSearch
# ============================================================================

[workspace]
resolver = "3"
members = [
    "crates/uffs-tui",
    "crates/uffs-gui",
    # "crates/uffs-forensics",   # uncomment when ready
]

[workspace.package]
version = "0.1.0"
edition = "2024"
rust-version = "1.91"
license = "LicenseRef-UFFS-Proprietary"
authors = ["SKY, LLC."]

# ── Public core dependencies (from the open-source UFFS repo) ──
[workspace.dependencies]
uffs-core   = { git = "https://github.com/githubrobbi/UltraFastFileSearch", branch = "main" }
uffs-client = { git = "https://github.com/githubrobbi/UltraFastFileSearch", branch = "main" }
uffs-polars = { git = "https://github.com/githubrobbi/UltraFastFileSearch", branch = "main" }
uffs-mft    = { git = "https://github.com/githubrobbi/UltraFastFileSearch", branch = "main" }
uffs-text   = { git = "https://github.com/githubrobbi/UltraFastFileSearch", branch = "main" }

# ── Shared third-party deps ──
tokio     = { version = "1.51", features = ["full"] }
clap      = { version = "4", features = ["derive"] }
anyhow    = "1"
tracing   = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
tracing-appender = "0.2"
ratatui   = "0.29"
crossterm = "0.28"
rayon     = "1.10"
ratatui-textarea = "0.7"
devicons  = "0.3"
regex     = "1"
serde     = { version = "1", features = ["derive"] }
toml      = "0.8"
dirs-next = "2"
```

#### Step 4: Create `uffs-products/Cargo.local.toml` (gitignored template)

```toml
# Copy this to your local Cargo.toml [patch] section for side-by-side dev.
# DO NOT commit — this file is gitignored.
#
# [patch."https://github.com/githubrobbi/UltraFastFileSearch"]
# uffs-core   = { path = "../UltraFastFileSearch/crates/uffs-core" }
# uffs-client = { path = "../UltraFastFileSearch/crates/uffs-client" }
# uffs-polars = { path = "../UltraFastFileSearch/crates/uffs-polars" }
# uffs-mft    = { path = "../UltraFastFileSearch/crates/uffs-mft" }
# uffs-text   = { path = "../UltraFastFileSearch/crates/uffs-text" }
```

#### Step 5: Move crate source from public to private repo

```bash
# Move uffs-tui
cp -r ../UltraFastFileSearch/crates/uffs-tui/src uffs-products/crates/uffs-tui/src

# Move uffs-gui
cp -r ../UltraFastFileSearch/crates/uffs-gui/src uffs-products/crates/uffs-gui/src
```

Then create new `Cargo.toml` files for each crate in the private repo, referencing
workspace dependencies instead of the public workspace. (See section 7.8 for the
per-crate Cargo.toml templates.)

#### Step 6: Remove from public workspace

In the public repo's `Cargo.toml`, remove `uffs-tui` and `uffs-gui` from `members`:

```toml
members = [
    # ── Foundation ──
    "crates/uffs-polars",
    "crates/uffs-security",
    "crates/uffs-text",
    "crates/uffs-mft",
    "crates/uffs-core",
    # ── Daemon Architecture ──
    "crates/uffs-daemon",
    "crates/uffs-client",
    "crates/uffs-mcp",
    "crates/uffs-broker",
    # ── Surfaces ──
    "crates/uffs-cli",
    # ── Tools ──
    "crates/uffs-diag",
]
```

Delete the `crates/uffs-tui/` and `crates/uffs-gui/` directories from the public repo.

#### Step 7: Verify both repos build

```bash
# Public repo
cd ~/Private/Github/UltraFastFileSearch
cargo check --workspace

# Private repo
cd ~/Private/Github/uffs-products
cargo check --workspace
```

### 7.5 Sample / demo binaries

The private repo produces paid binaries. To give users a taste, create **demo builds**
with limited functionality. These are built from the same source using Cargo feature flags.

#### Feature-flag strategy

In each paid crate's `Cargo.toml`:

```toml
[features]
default = ["demo"]     # Default build is demo (limited)
full = []              # Full/paid build — unlocks everything
demo = []              # Explicit demo flag
```

In the source code, gate paid features:

```rust
#[cfg(not(feature = "full"))]
const MAX_RESULTS: usize = 100;    // Demo: limited to 100 results

#[cfg(feature = "full")]
const MAX_RESULTS: usize = usize::MAX;  // Full: unlimited

#[cfg(not(feature = "full"))]
fn show_demo_banner() {
    eprintln!("╔══════════════════════════════════════════════════════════╗");
    eprintln!("║  UFFS TUI — Demo Version                                ║");
    eprintln!("║  Limited to 100 results per search.                     ║");
    eprintln!("║  Get the full version at https://uffs.dev/products       ║");
    eprintln!("╚══════════════════════════════════════════════════════════╝");
}
```

#### Building demo vs full

```bash
# Demo binary (default)
cargo build --release -p uffs-tui

# Full/paid binary
cargo build --release -p uffs-tui --features full --no-default-features
```

#### What to limit in demo versions

| Feature | Demo | Full |
|---------|------|------|
| Search results | Max 100 | Unlimited |
| Export/save | Disabled | All formats |
| Search history | Last 5 | Unlimited |
| Column customization | Disabled | Full |
| Keybinding presets | Default only | All presets |
| Startup banner | Shows demo notice | Clean |
| Tree view | Disabled | Full |
| Forensic timeline | N/A | Full (uffs-forensics) |

#### Distribution of demo binaries

Build demo binaries in CI from the private repo and publish them as:
- GitHub Releases on the **public** repo (as attached assets, or linked)
- Or a downloads page on `uffs.dev`

### 7.6 Referencing paid products from the public repo

In the public repo's `README.md`, add a section pointing to the paid products:

```markdown
## Products

UFFS Core is the open-source platform. Official products built on top:

| Product | Description | |
|---------|-------------|---|
| **UFFS TUI** | Interactive terminal search — real-time filtering, tree view, column customization | [Demo download](https://github.com/githubrobbi/UltraFastFileSearch/releases) |
| **UFFS Studio** | Native GUI — saved workspaces, charts, folder treemaps, duplicate review | Coming soon |
| **UFFS Forensics** | Timeline analysis, evidence packs, chain-of-custody exports | Coming soon |

Demo versions are free with limited functionality.
Full versions available at [uffs.dev/products](https://uffs.dev/products).
```

### 7.7 Repo scaffolding — inherit from the public repo

The private repo must inherit the same toolchain, lint, formatting, testing, and CI
standards. Here is every scaffolding file and what to do with it.

#### 7.7.1 Files to copy verbatim (identical in both repos)

| File | Purpose | Notes |
|------|---------|-------|
| `rust-toolchain.toml` | Pin nightly channel + components + targets | **Must be identical** — both repos must compile with the same nightly |
| `rustfmt.toml` | Nightly rustfmt settings (edition 2024, style_edition 2024) | Copy as-is, update copyright to `SKY, LLC.` |
| `clippy.toml` | Test relaxations, thresholds, cognitive complexity | Copy as-is |
| `.config/nextest.toml` | Nextest profiles (default, ci, fast, slow) | Copy as-is, update copyright |
| `.config/coverage.toml` | LLVM coverage exclusions | Copy as-is, update copyright |
| `deny.toml` | cargo-deny license allowlist + ban config | Copy as-is; add `allow-git` entry for the public UFFS repo |
| `audit.toml` | cargo-audit config | Copy as-is, update copyright |

#### 7.7.2 Workspace lint configuration

The public repo has a massive `[workspace.lints.clippy]` + `[workspace.lints.rust]` +
`[workspace.lints.rustdoc]` block in `Cargo.toml` (~100 lines of lint rules). The
private repo must replicate this in its own `Cargo.toml`.

Copy the entire `[workspace.lints.clippy]`, `[workspace.lints.rust]`, and
`[workspace.lints.rustdoc]` sections from the public `Cargo.toml` into the private
`Cargo.toml`. Each crate then inherits via `[lints] workspace = true`.

**Important:** Keep these in sync. When lint rules change in the public repo, update
the private repo to match. Consider extracting shared lint config into a documented
reference that both repos point to.

#### 7.7.3 Cargo profiles

Copy the `[profile.dev]`, `[profile.release]`, `[profile.xwin-dev]`, and any other
profile sections from the public `Cargo.toml`. The private repo needs the same
optimization and debug settings.

#### 7.7.4 Justfile and just/ recipes

The private repo needs its own `justfile` and `just/` directory, adapted for its crates:

```
uffs-products/
├── justfile              ← orchestrator (imports from just/)
└── just/
    ├── shared.just       ← copy from public repo (env vars, flags, install helpers)
    ├── test.just          ← adapted: test only private workspace crates
    ├── build.just         ← adapted: build demo + full binaries
    ├── dev.just           ← adapted: watch mode for private crates
    └── workflow.just      ← adapted: go / ship pipeline for private repo
```

What to keep identical:
- `shared.just` — shell settings, env vars, clippy flags, `_install-if-missing` helper
- Lint flags (`common_flags`, `prod_flags`, `test_flags`)
- Tool installation recipes (`setup`)

What to adapt:
- `build.just` — add `build-demo` and `build-full` recipes
- `workflow.just` — simpler pipeline (no version bump scripts, no public release)
- Remove recipes that don't apply (e.g., `polars`, `cross-deploy`, `quick-deploy`)
- Add `build-demo` and `build-full` recipes

Example new recipes for the private repo:

```just
# Build demo binaries (default features = demo)
build-demo:
    @printf "\033[0;34m🔨 Building demo binaries...\033[0m\n"
    cargo build --release --workspace
    @printf "\033[0;32m✅ Demo binaries built\033[0m\n"

# Build full/paid binaries
build-full:
    @printf "\033[0;34m🔨 Building full (paid) binaries...\033[0m\n"
    cargo build --release --workspace --features full --no-default-features
    @printf "\033[0;32m✅ Full binaries built\033[0m\n"
```

#### 7.7.5 GitHub Actions CI

Create `.github/workflows/ci.yml` in the private repo, modeled on the public one.
Key differences:

- Runs on private repo pushes only (no public PRs)
- Builds demo + full feature variants
- Does NOT publish to crates.io
- Can publish demo binaries as GitHub Release artifacts

```yaml
# uffs-products CI
# Copyright 2025-2026 SKY, LLC.
# SPDX-License-Identifier: LicenseRef-UFFS-Proprietary

name: 🧪 UFFS Products CI

on:
  push:
    branches: [main]
  pull_request:
    branches: [main]

env:
  CARGO_TERM_COLOR: always
  RUST_BACKTRACE: 1

jobs:
  check:
    name: Check + Lint + Test
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@nightly
        with:
          components: rustfmt, clippy
      - name: Check (demo)
        run: cargo check --workspace
      - name: Check (full)
        run: cargo check --workspace --features full --no-default-features
      - name: Format
        run: cargo fmt --all -- --check
      - name: Clippy
        run: cargo clippy --workspace --all-features -- -D warnings
      - name: Test
        run: cargo nextest run --workspace --all-features
```

#### 7.7.6 `deny.toml` adaptation

The private repo's `deny.toml` needs one addition — allow the public UFFS repo as a git
source:

```toml
[sources]
unknown-registry = "warn"
unknown-git = "warn"
allow-registry = ["https://github.com/rust-lang/crates.io-index"]
allow-git = [
    "https://github.com/pola-rs/polars",
    "https://github.com/githubrobbi/UltraFastFileSearch",
]
```

#### 7.7.7 CLAUDE.md / AI assistant config

Create a `CLAUDE.md` in the private repo with:
- Reference to the public repo for core architecture docs
- Note that this is a proprietary repo with commercial products
- Same build/test commands adapted for the private workspace
- Reminder to never commit `[patch]` overrides

#### 7.7.8 Private repo `.gitignore`

```gitignore
# SPDX-License-Identifier: LicenseRef-UFFS-Proprietary
# Copyright (c) 2025-2026 SKY, LLC.

# Rust / Cargo
/target
**/*.rs.bk
*.pdb

# Never commit patch overrides for local dev
Cargo.local.toml

# Build artifacts
/build/
/dist/

# IDE
.idea/
.vscode/
*.swp
*.swo

# OS
.DS_Store
Thumbs.db

# Logs
/LOG/
*.log
```

#### 7.7.9 Complete private repo directory structure

```
uffs-products/
├── .config/
│   ├── coverage.toml           ← copy from public
│   └── nextest.toml            ← copy from public
├── .github/
│   └── workflows/
│       └── ci.yml              ← adapted CI
├── .gitignore
├── Cargo.toml                  ← workspace root (see section 7.4)
├── Cargo.local.toml            ← template for [patch] overrides (gitignored)
├── CLAUDE.md                   ← AI assistant guidance
├── LICENSE                     ← proprietary license
├── README.md                   ← internal: what this repo is, how to build
├── audit.toml                  ← copy from public
├── clippy.toml                 ← copy from public
├── crates/
│   ├── uffs-tui/
│   │   ├── Cargo.toml
│   │   └── src/                ← moved from public repo
│   ├── uffs-gui/
│   │   ├── Cargo.toml
│   │   └── src/                ← moved from public repo
│   └── uffs-forensics/
│       ├── Cargo.toml
│       └── src/
├── deny.toml                   ← adapted (allow public repo git source)
├── just/
│   ├── shared.just             ← copy from public
│   ├── test.just               ← adapted
│   ├── build.just              ← adapted (demo + full recipes)
│   ├── dev.just                ← adapted
│   └── workflow.just           ← adapted
├── justfile                    ← orchestrator
├── release-plz.toml            ← adapted (no crates.io publish)
├── rust-toolchain.toml         ← copy from public (identical)
└── rustfmt.toml                ← copy from public
```

### 7.8 Per-crate `Cargo.toml` templates for the private repo

**`uffs-products/crates/uffs-tui/Cargo.toml`:**

```toml
[package]
name = "uffs-tui"
description = "Terminal UI for UFFS — commercial product by SKY, LLC."
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
authors.workspace = true

[[bin]]
name = "uffs_tui"
path = "src/main.rs"

[features]
default = ["demo"]
full = []
demo = []

[dependencies]
uffs-core.workspace = true
uffs-client.workspace = true
uffs-polars.workspace = true
uffs-text.workspace = true
tokio.workspace = true
clap.workspace = true
ratatui.workspace = true
crossterm.workspace = true
rayon.workspace = true
ratatui-textarea.workspace = true
devicons.workspace = true
regex.workspace = true
serde.workspace = true
toml.workspace = true
anyhow.workspace = true
tracing.workspace = true
tracing-subscriber.workspace = true
tracing-appender.workspace = true
dirs-next.workspace = true
```

**`uffs-products/crates/uffs-gui/Cargo.toml`:**

```toml
[package]
name = "uffs-gui"
description = "Graphical UI for UFFS — commercial product by SKY, LLC."
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
authors.workspace = true

[[bin]]
name = "uffs_gui"
path = "src/main.rs"

[features]
default = ["demo"]
full = []
demo = []

[dependencies]
uffs-client.workspace = true
tokio.workspace = true
anyhow.workspace = true
tracing.workspace = true
tracing-subscriber.workspace = true
clap.workspace = true
```

### 7.9 Local development workflow (side-by-side)

Use `[patch]` in the private repo so you don't have to push core changes before testing:

```toml
# Add to uffs-products/Cargo.toml temporarily during local dev
[patch."https://github.com/githubrobbi/UltraFastFileSearch"]
uffs-core   = { path = "../UltraFastFileSearch/crates/uffs-core" }
uffs-client = { path = "../UltraFastFileSearch/crates/uffs-client" }
uffs-polars = { path = "../UltraFastFileSearch/crates/uffs-polars" }
uffs-mft    = { path = "../UltraFastFileSearch/crates/uffs-mft" }
uffs-text   = { path = "../UltraFastFileSearch/crates/uffs-text" }
```

Workflow:
1. Edit core in `UltraFastFileSearch/` — Cargo resolves via `[patch]` immediately
2. Edit product in `uffs-products/` — builds against local core
3. When done: commit + push core, remove `[patch]`, `cargo update` in products repo
4. Commit + push products

### 7.10 Graduation path

| Phase | Mechanism | When |
|-------|-----------|------|
| Phase 1 | Git dependencies (`git = "..."`) | Now — fast iteration |
| Phase 2 | Publish core crates to crates.io | When APIs stabilize |
| Phase 3 | Private Cargo registry for paid crates | When selling enterprise |

---

## 8. Execution Checklist

### Phase 1: License Cleanup (do first, do now)

- [ ] Delete `LICENSES/LicenseRef-Proprietary.txt`
- [ ] Delete `LICENSES/LicenseRef-SKY-Proprietary.txt`
- [ ] Delete `LICENSES/LicenseRef-TTAPI-Commercial.txt`
- [ ] Delete `LICENSES/Apache-2.0.txt` (or keep if planning Apache docs later)
- [ ] Delete `LICENSES/MIT.txt` (or keep if planning MIT schemas later)
- [ ] Decide: create `LICENSES/LicenseRef-UFFS-Commercial.txt` now or defer
- [ ] Rewrite root `LICENSE` — remove TTAPI/SKY/dual-license noise
- [ ] Rewrite `REUSE.toml` — change default to `MPL-2.0`
- [ ] Update `Cargo.toml` line 51 — either simplify to `MPL-2.0` or create the commercial file
- [ ] Fix SPDX headers in `.config/coverage.toml`, `.config/nextest.toml`, `rustfmt.toml`
- [ ] Add SPDX headers to all `.rs` files in `crates/`
- [ ] Add SPDX header to `Justfile`, `build.rs`, and any other root files

### Phase 2: README Update

- [ ] Add prominent "Open source, forever" block after badges
- [ ] Expand the License section at the bottom with MPL explanation
- [ ] Remove or update any references to commercial licensing in docs

### Phase 3: Create Private Repo and Move Commercial Crates

#### 3a. Create repo and scaffolding (see section 7.7)

- [ ] Create private GitHub repo `uffs-products`
- [ ] Create workspace `Cargo.toml` with git deps pointing to public repo (section 7.4)
- [ ] Copy `[workspace.lints.clippy]`, `[workspace.lints.rust]`, `[workspace.lints.rustdoc]` from public `Cargo.toml`
- [ ] Copy `[profile.dev]`, `[profile.release]`, and other profile sections from public `Cargo.toml`
- [ ] Copy `rust-toolchain.toml` verbatim (must be identical)
- [ ] Copy `rustfmt.toml` (update copyright to SKY, LLC.)
- [ ] Copy `clippy.toml` verbatim
- [ ] Copy `.config/nextest.toml` (update copyright)
- [ ] Copy `.config/coverage.toml` (update copyright)
- [ ] Copy `deny.toml` (add public UFFS repo to `allow-git`)
- [ ] Copy `audit.toml` (update copyright)
- [ ] Create `.gitignore` (section 7.7.8)
- [ ] Create `Cargo.local.toml` template for local `[patch]` overrides
- [ ] Copy `just/shared.just` verbatim
- [ ] Create adapted `justfile`, `just/test.just`, `just/build.just`, `just/dev.just`, `just/workflow.just`
- [ ] Create `.github/workflows/ci.yml` (section 7.7.5)
- [ ] Create `CLAUDE.md` for AI assistant context
- [ ] Create `LICENSE` (proprietary)
- [ ] Create `README.md` (internal: what this repo is, how to build)

#### 3b. Move crate source

- [ ] Copy `crates/uffs-tui/src/` to private repo
- [ ] Copy `crates/uffs-gui/src/` to private repo
- [ ] Create per-crate `Cargo.toml` files in private repo (section 7.8)
- [ ] Add feature flags (`demo` / `full`) to each paid crate
- [ ] Add demo-mode gating (result limits, banner, disabled features)

#### 3c. Verify builds

- [ ] Verify private repo builds with `[patch]` overrides (local core)
- [ ] Push core changes to public repo
- [ ] Remove `[patch]` overrides, verify private repo builds against git deps
- [ ] Run `just go` (or equivalent) in private repo — fmt, lint, test pass

#### 3d. Clean up public repo

- [ ] Remove `crates/uffs-tui/` and `crates/uffs-gui/` from public repo
- [ ] Remove `uffs-tui` and `uffs-gui` from public workspace `members`
- [ ] Remove TUI/GUI-specific workspace dependencies from public `Cargo.toml`
- [ ] Verify public repo builds clean: `cargo check --workspace`
- [ ] Run `just go` in public repo — everything must still pass

#### 3e. Update public repo references

- [ ] Add "Products" section to public README linking to demo downloads
- [ ] Update public README Architecture table (remove TUI/GUI rows, add link)
- [ ] Tag public repo as the first clean-license release

#### 3f. Private repo CI and releases

- [ ] Set up CI in private repo (build demo + full binaries)
- [ ] Publish demo binaries (GitHub Releases or downloads page)

### Phase 4: Future Commercial Setup

- [ ] Write actual `LicenseRef-UFFS-Commercial.txt` terms (with counsel)
- [ ] Create `COMMERCIAL.md` explaining commercial options
- [ ] Create `TRADEMARKS.md` protecting UFFS name and logos
- [ ] Set up CLA for core contributions (if accepting external PRs)
- [ ] Publish `LICENSE-MATRIX.md` with path-level license map
- [ ] Create `uffs-forensics` crate in private repo
- [ ] Build product landing page at `uffs.dev/products`

---

## 9. What NOT to do

- **Do not keep SKY/TTAPI references anywhere.** Every one is a trust red flag.
- **Do not split the license header per-crate.** All public crates are MPL-2.0. One license.
- **Do not add commercial license terms until they are real.** A placeholder is worse than nothing.
- **Do not delay the cleanup.** The longer contradictory licenses sit in a public repo, the more
  damage they do to credibility.
- **Do not create `LICENSE-COMMERCIAL` without counsel.** Bad commercial terms are worse than
  no commercial terms.
