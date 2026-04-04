# Verify Rust toolchain consistency.
verify-toolchain:
    #!/usr/bin/env bash
    set -euo pipefail
    printf "\033[0;34m🔧 Verifying Rust toolchain consistency...\033[0m\n"
    rustc --version
    cargo --version
    rustfmt --version
    cargo clippy --version
    cargo metadata --format-version 1 --no-deps >/dev/null
    printf "\033[0;32m✅ Nightly toolchain verified (via rust-toolchain.toml)\033[0m\n"

# Format code (nightly rustfmt with edition 2024 + style_edition 2024).
fmt:
    @printf "\033[0;34m📝 Formatting code (nightly rustfmt, style_edition 2024)...\033[0m\n"
    cargo fmt --all
    @printf "\033[0;32m✅ Formatting complete\033[0m\n"

# Check code formatting without applying fixes.
fmt-check:
    @printf "\033[0;34m📝 Checking code formatting (nightly rustfmt, style_edition 2024)...\033[0m\n"
    cargo fmt --all -- --check

# Run all tests with nextest.
test:
    @printf "\033[0;34m🧪 Running all tests with nextest (FAST-FAIL)...\033[0m\n"
    cargo llvm-cov nextest run --workspace --all-features --all-targets --jobs 16 --no-report

# Run documentation tests separately.
test-doc:
    @printf "\033[0;34m📚 Running documentation tests (FAST-FAIL)...\033[0m\n"
    RUSTDOCFLAGS='-Dwarnings -Dmissing_docs' cargo llvm-cov test --doc --workspace --all-features --no-report

# Run slow or ignored tests.
test-slow:
    @printf "\033[0;34m🐌 Running slow/ignored tests (comprehensive validation)...\033[0m\n"
    @printf "\033[1;33m⚠️  These tests may take several minutes - includes full workspace validation\033[0m\n"
    cargo nextest run --workspace --all-features --all-targets --profile slow --run-ignored ignored-only

# Generate coverage report.
coverage:
    @printf "\033[0;34m📊 Generating coverage report...\033[0m\n"
    cargo clean
    @if command -v cargo-llvm-cov >/dev/null 2>&1; then \
        cargo llvm-cov nextest --workspace --all-features --all-targets --jobs 16 --html; \
        printf "\033[0;32m📁 Coverage report: target/llvm-cov/html/index.html\033[0m\n"; \
    else \
        printf "\033[1;33mInstalling cargo-llvm-cov...\033[0m\n"; \
        just _install-if-missing cargo-llvm-cov cargo-llvm-cov; \
        cargo llvm-cov nextest --workspace --all-features --all-targets --jobs 16 --html; \
        printf "\033[0;32m📁 Coverage report: target/llvm-cov/html/index.html\033[0m\n"; \
    fi

# Ultra-strict production linting (workspace lints + justfile overrides).
lint-prod:
    @printf "\033[0;34m🔍 Ultra-strict production linting (FAST-FAIL)...\033[0m\n"
    cargo clippy --workspace --lib --bins --all-features --no-deps -- {{ prod_flags }}

# Pragmatic test linting (allows unwrap/expect in test code).
lint-tests:
    @printf "\033[0;34m🧪 Pragmatic test linting (FAST-FAIL)...\033[0m\n"
    cargo clippy --workspace --tests --all-features --no-deps -- {{ test_flags }}

# Gate residual lints in CI (deny all warnings).
lint-ci:
    @printf "\033[0;34m🚨 CI lint gating (FAST-FAIL)...\033[0m\n"
    cargo clippy --workspace --all-features -- -D warnings

# Full lint sweep: production + tests + CI gate + formatting.
lint-all:
    @printf "\033[0;34m🔍 Full lint sweep (prod + tests + CI + fmt)...\033[0m\n"
    just lint-prod
    just lint-tests
    just lint-ci
    just fmt-check
    @printf "\033[0;32m✅ All lint checks passed\033[0m\n"

# Enforce oversized Rust file policy with explicit exceptions.
file-size-policy:
    @printf "\033[0;34m📏 Checking oversized Rust file policy...\033[0m\n"
    bash scripts/ci/check_file_size_policy.sh

# Comprehensive code validation (CI/CD grade).
check:
    @printf "\033[0;34m🔍 Comprehensive validation (CI/CD grade)...\033[0m\n"
    just file-size-policy
    cargo check --workspace --all-targets --all-features
    just fmt-check
    @printf "\033[0;32m✅ Comprehensive check passed\033[0m\n"

# Quick code validation.
check-quick:
    @printf "\033[0;34m⚡ Quick validation (development speed)...\033[0m\n"
    cargo check --workspace

# Bump Polars to latest main HEAD (updates rev= hash, refreshes lockfile, checks build).
polars:
    #!/usr/bin/env bash
    set -euo pipefail
    printf "\033[0;34m📦 Bumping Polars to latest main HEAD...\033[0m\n"

    NEW_REV=$(git ls-remote https://github.com/pola-rs/polars refs/heads/main | awk '{print $1}')
    if [ -z "$NEW_REV" ]; then
        printf "\033[0;31m❌ Failed to fetch latest Polars commit\033[0m\n"
        exit 1
    fi

    OLD_REV=$(sed -n 's/.*rev = "\([a-f0-9]*\)".*/\1/p' crates/uffs-polars/Cargo.toml)
    if [ "$OLD_REV" = "$NEW_REV" ]; then
        printf "\033[0;32m✅ Already at latest: %s\033[0m\n" "${NEW_REV:0:12}"
        exit 0
    fi

    printf "   Old: %s\n" "${OLD_REV:0:12}"
    printf "   New: %s\n" "${NEW_REV:0:12}"

    # macOS sed requires a space after -i, Linux doesn't care — use backup + remove
    sed -i.bak "s|rev = \"${OLD_REV}\"|rev = \"${NEW_REV}\"|" crates/uffs-polars/Cargo.toml
    rm -f crates/uffs-polars/Cargo.toml.bak
    cargo update -p polars
    printf "\033[0;34m🔨 Checking build...\033[0m\n"
    cargo check -p uffs-polars
    printf "\033[0;32m✅ Polars bumped to %s\033[0m\n" "${NEW_REV:0:12}"

# Update all workspace dependencies to latest compatible versions.
update-deps:
    @printf "\033[0;34m📦 Updating all workspace dependencies...\033[0m\n"
    cargo update
    @printf "\033[0;32m✅ All dependencies updated\033[0m\n"

