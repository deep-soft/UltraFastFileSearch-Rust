# Watch mode development.
dev:
    @printf "\033[0;34m🔄 Starting watch mode...\033[0m\n"
    @if command -v cargo-watch >/dev/null 2>&1; then \
        cargo watch -x "nextest run --workspace --jobs 16" -x "clippy --workspace --all-targets --all-features --no-deps -- {{ test_flags }}"; \
    else \
        printf "\033[1;33mInstalling cargo-watch...\033[0m\n"; \
        just _install-if-missing cargo-watch cargo-watch; \
        cargo watch -x "nextest run --workspace --jobs 16" -x "clippy --workspace --all-targets --all-features --no-deps -- {{ test_flags }}"; \
    fi

# Clean build artifacts.
clean:
    @printf "\033[0;34m🧹 Cleaning build artifacts...\033[0m\n"
    cargo clean

# Emergency killswitch.
kill:
    @printf "\033[0;31m🚨 EMERGENCY: Killing all cargo processes...\033[0m\n"
    @pkill -f "cargo" 2>/dev/null || true
    @pkill -f "rustc" 2>/dev/null || true
    @printf "\033[0;32m✅ All cargo processes terminated\033[0m\n"

# Install cross-compilation targets for GitHub Actions compatibility.
setup-cross:
    @printf "\033[0;34m Installing cross-compilation targets...\033[0m\n"
    rustup target add x86_64-unknown-linux-gnu
    rustup target add aarch64-unknown-linux-gnu
    rustup target add x86_64-pc-windows-msvc
    @printf "\033[0;32m✅ Cross-compilation targets installed\033[0m\n"

# Cross-compile check for GitHub Actions compatibility.
check-cross:
    @printf "\033[0;34m Cross-compilation validation using CI pipeline...\033[0m\n"
    rust-script scripts/ci/ci-pipeline.rs cross-check

# Install core development tools.
devenv-tools:
    @printf "\033[0;34m🛠️ Installing core development tools...\033[0m\n"
    cargo install cargo-nextest
    cargo install cargo-llvm-cov
    cargo install cargo-audit
    cargo install cargo-deny
    cargo install cargo-semver-checks
    @printf "\033[0;32m✅ Core tools installed\033[0m\n"

# Run all tests (alias for test).
devenv-test-all: test
    @printf "\033[0;32m✅ All tests complete\033[0m\n"

# Install all modern development tools.
install-tools:
    @printf "\033[0;34m🔧 Installing modern development tools...\033[0m\n"
    @printf "\033[1;33mUsing cargo-binstall for fast installation...\033[0m\n"
    cargo binstall cargo-nextest cargo-llvm-cov cargo-audit cargo-watch just --no-confirm
    @printf "\033[0;32m✅ All modern tools installed successfully!\033[0m\n"

# Update all development tools to latest versions.
update-tools:
    @printf "\033[0;34m🔄 Updating development tools...\033[0m\n"
    @if command -v cargo-install-update >/dev/null 2>&1; then \
        cargo install-update -a; \
    else \
        printf "\033[1;33mInstalling cargo-update for efficient updates...\033[0m\n"; \
        just _install-if-missing cargo-update cargo-update; \
        cargo install-update -a; \
    fi
    @printf "\033[0;32m✅ All tools updated to latest versions!\033[0m\n"



# Quick deploy to Windows: version bump → xcompile → commit → push.
# Skips tests, lint, coverage, and macOS native builds. For debug iteration only.
# Alias: just q
[unix]
q: quick-deploy

[unix]
quick-deploy:
    #!/usr/bin/env bash
    set -euo pipefail
    TOTAL_START=$(date +%s)
    printf "\033[1;34m⚡ Quick Deploy to Windows (skip CI)\033[0m\n"
    printf "\033[1;33m⚠️  No tests, no linting — debug iteration only!\033[0m\n"
    echo "========================================================"

    # 1. Version bump
    printf "\n\033[0;34m📈 Step 1/4: Version bump...\033[0m\n"
    if [ -f "./build/update_all_versions.rs" ]; then
        ./build/update_all_versions.rs patch
    else
        printf "\033[1;31m❌ Version script not found\033[0m\n"
        exit 1
    fi
    NEW_VERSION=$(awk '/^\[workspace\.package\]/{flag=1; next} flag && /^version/{print $3; exit}' Cargo.toml | tr -d '"')
    printf "\033[0;32m  → v%s\033[0m\n" "$NEW_VERSION"

    # Sync workflow state so build-cross-all.rs uses the correct version
    STATE_FILE="build/.uffs-workflow-state.json"
    if [ -f "$STATE_FILE" ]; then
        # Update current_version in-place using portable sed
        sed -i.bak "s/\"current_version\": *\"[^\"]*\"/\"current_version\": \"${NEW_VERSION}\"/" "$STATE_FILE"
        rm -f "${STATE_FILE}.bak"
        printf "\033[0;36m  → Synced workflow state to v%s\033[0m\n" "$NEW_VERSION"
    fi

    # 2. Cross-compile for Windows (build-cross-all.rs handles dist/ copy)
    printf "\n\033[0;34m🔨 Step 2/4: Cross-compile for Windows...\033[0m\n"
    START=$(date +%s)
    rust-script scripts/ci/build-cross-all.rs
    END=$(date +%s)
    printf "\033[0;32m  → Build completed in %ds\033[0m\n" $((END - START))

    # 3. Git commit
    printf "\n\033[0;34m📝 Step 3/4: Git commit...\033[0m\n"
    git add .
    git -c commit.gpgsign=false commit -m "debug: quick-deploy v${NEW_VERSION} [skip ci]"
    printf "\033[0;32m  → Committed\033[0m\n"

    # 4. Git push
    printf "\n\033[0;34m🚀 Step 4/4: Git push...\033[0m\n"
    git pull origin main --rebase
    git push origin main
    printf "\033[0;32m  → Pushed\033[0m\n"

    TOTAL_END=$(date +%s)
    ELAPSED=$((TOTAL_END - TOTAL_START))
    MINS=$((ELAPSED / 60))
    SECS=$((ELAPSED % 60))
    printf "\n\033[1;32m⚡ Quick deploy complete: v%s in %dm %ds\033[0m\n" "$NEW_VERSION" "$MINS" "$SECS"
    printf "\033[0;34m💡 On Windows: git pull && just use\033[0m\n"