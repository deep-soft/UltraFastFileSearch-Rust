# SPDX-License-Identifier: MPL-2.0
# Copyright (c) 2025-2026 SKY, LLC.

# Shared settings, flags, helpers, and bootstrap.
# Updated: April 2026 — Nightly (1.96+) / Edition 2024

set shell := ["bash", "-euo", "pipefail", "-c"]
set windows-shell := ["powershell.exe", "-NoLogo", "-NoProfile", "-Command"]

export FORCE_COLOR := "1"
export CLICOLOR_FORCE := "1"
export TERM := "xterm-256color"
export COLORTERM := "truecolor"
export CARGO_TERM_COLOR := "always"
export CARGO_INCREMENTAL := "1"
export CARGO_NET_RETRY := "3"
export CARGO_HTTP_TIMEOUT := "30"
export CARGO_HTTP_MULTIPLEXING := "true"
export RUSTC_WRAPPER := ""

# Clippy flags — supplement workspace.lints (belt-and-suspenders for CI).
# Workspace Cargo.toml [workspace.lints] is the source of truth; these ensure
# the justfile lint recipes stay strict even if workspace lints drift.
common_flags := "-D clippy::pedantic -D clippy::nursery -D clippy::cargo -A clippy::multiple_crate_versions -A clippy::redundant_pub_crate -W clippy::panic -W clippy::todo -W clippy::unimplemented -D warnings"
prod_flags := common_flags + " -W clippy::unwrap_used -W clippy::expect_used -W clippy::missing_docs_in_private_items"
test_flags := common_flags + " -A clippy::unwrap_used -A clippy::expect_used"

# Install cargo tool if missing (idempotent)
_install-if-missing TOOL CRATE:
    #!/usr/bin/env bash
    if ! command -v {{ TOOL }} >/dev/null 2>&1; then
        printf "\033[0;34m📦 Installing {{ CRATE }}...\033[0m\n"
        if command -v cargo-binstall >/dev/null 2>&1; then
            cargo binstall {{ CRATE }} --no-confirm --quiet
        else
            cargo install {{ CRATE }} --locked --quiet
        fi
        printf "\033[0;32m✅ {{ TOOL }} installed\033[0m\n"
    else
        printf "\033[0;32m✅ {{ TOOL }} already installed (skip)\033[0m\n"
    fi

# Install rustup component if missing (idempotent)
_install-component COMPONENT:
    #!/usr/bin/env bash
    if ! rustup component list --installed | grep -q "{{ COMPONENT }}"; then
        printf "\033[0;34m📦 Installing {{ COMPONENT }} component...\033[0m\n"
        rustup component add {{ COMPONENT }}
        printf "\033[0;32m✅ {{ COMPONENT }} component installed\033[0m\n"
    else
        printf "\033[0;32m✅ {{ COMPONENT }} component already installed (skip)\033[0m\n"
    fi

# Ensure the pinned nightly toolchain (from rust-toolchain.toml) is installed.
# Called automatically by the CI pipeline. Does NOT change the pinned version.
toolchain-ensure:
    #!/usr/bin/env bash
    set -euo pipefail
    TOOLCHAIN_FILE="rust-toolchain.toml"
    CURRENT=$(grep '^channel' "$TOOLCHAIN_FILE" | sed 's/.*"\(.*\)".*/\1/')
    printf "\033[0;34m🔧 Ensuring toolchain %s is installed...\033[0m\n" "$CURRENT"
    if ! rustup toolchain install "$CURRENT" --profile default 2>&1; then
        printf "\033[0;33m⚠️  Install failed — clearing rustup download cache and retrying...\033[0m\n"
        rm -rf ~/.rustup/downloads/*
        rustup toolchain install "$CURRENT" --profile default
    fi
    rustup default "$CURRENT"
    printf "\033[0;32m✅ Toolchain %s ready\033[0m\n" "$CURRENT"
    rustc --version

# Sync the nightly toolchain to the latest date and update rust-toolchain.toml.
# Run this MANUALLY when you want to bump the pinned nightly (e.g. before `just go`).
# Do NOT call from CI — use `toolchain-ensure` instead.
toolchain-sync:
    #!/usr/bin/env bash
    set -euo pipefail
    printf "\033[0;34m🔧 Syncing to latest nightly toolchain...\033[0m\n"

    # Determine today's date for the nightly channel.
    TODAY=$(date -u +%Y-%m-%d)
    NEW_CHANNEL="nightly-${TODAY}"
    TOOLCHAIN_FILE="rust-toolchain.toml"

    # Read the current channel.
    CURRENT=$(grep '^channel' "$TOOLCHAIN_FILE" | sed 's/.*"\(.*\)".*/\1/')
    if [ "$CURRENT" = "$NEW_CHANNEL" ]; then
        printf "\033[0;32m✅ Already on latest nightly (%s)\033[0m\n" "$NEW_CHANNEL"
        rustc --version
        exit 0
    fi

    printf "\033[0;34m   %s → %s\033[0m\n" "$CURRENT" "$NEW_CHANNEL"

    # Update the file.
    sed -i'' -e "s/channel = \"${CURRENT}\"/channel = \"${NEW_CHANNEL}\"/" "$TOOLCHAIN_FILE"
    # Update the comment date.
    sed -i'' -e "s/# Latest nightly — updated .*/# Latest nightly — updated $(date -u +%Y-%m-%d)./" "$TOOLCHAIN_FILE"

    # Install the new toolchain (rustup reads rust-toolchain.toml automatically).
    # Retry with cache clear on failure (stale .partial files cause rename errors).
    printf "\033[0;34m   Installing %s...\033[0m\n" "$NEW_CHANNEL"
    if ! rustup toolchain install "$NEW_CHANNEL" --profile default 2>&1; then
        printf "\033[0;33m⚠️  Install failed — clearing rustup download cache and retrying...\033[0m\n"
        rm -rf ~/.rustup/downloads/*
        rustup toolchain install "$NEW_CHANNEL" --profile default
    fi
    rustup default "$NEW_CHANNEL"

    # Verify.
    printf "\033[0;32m✅ Toolchain synced to %s\033[0m\n" "$NEW_CHANNEL"
    rustc --version
    cargo --version

# First-time development environment setup (install-if-missing only).
# For updates, use `just update-tools`.
setup:
    #!/usr/bin/env bash
    printf "\033[0;34m🔧 Development Environment Setup\033[0m\n"
    printf "\033[0;34m   Nightly toolchain + essential Rust development tools\033[0m\n"
    echo "========================================================"
    just _install-if-missing cargo-binstall cargo-binstall
    just _install-if-missing cargo-nextest cargo-nextest
    just _install-if-missing cargo-llvm-cov cargo-llvm-cov
    just _install-if-missing cargo-deny cargo-deny
    just _install-if-missing cargo-xwin cargo-xwin
    just _install-if-missing cargo-machete cargo-machete
    just _install-if-missing cargo-outdated cargo-outdated
    just _install-if-missing cargo-semver-checks cargo-semver-checks
    just _install-if-missing cargo-mutants cargo-mutants
    just _install-if-missing cargo-insta cargo-insta
    just _install-if-missing cargo-watch cargo-watch
    just _install-if-missing cargo-expand cargo-expand
    just _install-if-missing rust-script rust-script
    just _install-if-missing sccache sccache
    just _install-if-missing flamegraph flamegraph
    just _install-if-missing samply samply
    just _install-component llvm-tools
    just _install-component miri
    printf "\033[0;32m✅ Development environment setup complete!\033[0m\n"
    printf "\033[0;34mToolchain: $(rustc --version)\033[0m\n"
    printf "\033[0;34mNext steps:\033[0m\n"
    printf "  • Run 'just update-tools' to update everything to latest\n"
    printf "  • Run 'just go' to validate everything safely\n"
    printf "  • Run 'just ship' for the full ship pipeline\n"

