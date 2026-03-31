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



# Quick deploy to Windows: version bump → xcompile → deploy to dist/ → commit → push.
# Skips all CI validation (tests, lint, coverage). For debug iteration only.
[unix]
quick-deploy:
    #!/usr/bin/env bash
    set -euo pipefail
    printf "\033[1;34m⚡ Quick Deploy to Windows (skip CI)\033[0m\n"
    printf "\033[1;33m⚠️  No tests, no linting — debug iteration only!\033[0m\n"
    echo "========================================================"

    # 1. Version bump
    printf "\n\033[0;34m📈 Step 1/5: Version bump...\033[0m\n"
    if [ -f "./build/update_all_versions.rs" ]; then
        ./build/update_all_versions.rs patch
    else
        printf "\033[1;31m❌ Version script not found\033[0m\n"
        exit 1
    fi
    NEW_VERSION=$(awk '/^\[workspace\.package\]/{flag=1; next} flag && /^version/{print $3; exit}' Cargo.toml | tr -d '"')
    printf "\033[0;32m  → v%s\033[0m\n" "$NEW_VERSION"

    # 2. Cross-compile for Windows
    printf "\n\033[0;34m🔨 Step 2/5: Cross-compile for Windows...\033[0m\n"
    START=$(date +%s)
    rust-script scripts/ci/build-cross-all.rs
    END=$(date +%s)
    printf "\033[0;32m  → Build completed in %ds\033[0m\n" $((END - START))

    # 3. Deploy to dist/ and ~/bin
    printf "\n\033[0;34m📦 Step 3/5: Deploy artifacts...\033[0m\n"
    DIST_DIR="dist/v${NEW_VERSION}"
    mkdir -p "$DIST_DIR"

    # Copy cross-compiled Windows binaries
    WIN_TARGET="target/x86_64-pc-windows-msvc/release"
    if [ -d "$WIN_TARGET" ]; then
        for BIN in uffs uffs_tui uffs_gui uffs_mft; do
            SRC="$WIN_TARGET/${BIN}.exe"
            if [ -f "$SRC" ]; then
                BIN_DIST="$DIST_DIR/$BIN"
                mkdir -p "$BIN_DIST"
                cp "$SRC" "$BIN_DIST/${BIN}-windows-x64.exe"
                SIZE=$(du -sh "$SRC" | cut -f1)
                printf "  \033[0;32m✅ %-20s %s\033[0m\n" "${BIN}.exe" "$SIZE"
            fi
        done
    fi

    # Update dist/latest symlink
    rm -f dist/latest
    ln -sf "v${NEW_VERSION}" dist/latest
    printf "  \033[0;32m→ dist/latest → v%s\033[0m\n" "$NEW_VERSION"

    # Also install macOS native binaries to ~/bin
    printf "\n  \033[0;34mInstalling macOS binaries to ~/bin...\033[0m\n"
    cargo build --release -p uffs-cli -p uffs-daemon -p uffs-mcp 2>&1 | tail -3
    BIN_DIR="$HOME/bin"
    mkdir -p "$BIN_DIR"
    for BIN in uffs uffs-daemon uffs-mcp; do
        SRC="target/release/$BIN"
        if [ -f "$SRC" ]; then
            cp "$SRC" "$BIN_DIR/$BIN"
            chmod +x "$BIN_DIR/$BIN"
            printf "  \033[0;32m✅ ~/bin/%-16s\033[0m\n" "$BIN"
        fi
    done

    # 4. Git commit
    printf "\n\033[0;34m📝 Step 4/5: Git commit...\033[0m\n"
    git add .
    git -c commit.gpgsign=false commit -m "debug: quick-deploy v${NEW_VERSION} [skip ci]"
    printf "\033[0;32m  → Committed\033[0m\n"

    # 5. Git push
    printf "\n\033[0;34m🚀 Step 5/5: Git push...\033[0m\n"
    git pull origin main --rebase
    git push origin main
    printf "\033[0;32m  → Pushed\033[0m\n"

    printf "\n\033[1;32m⚡ Quick deploy complete: v%s pushed to remote\033[0m\n" "$NEW_VERSION"
    printf "\033[0;34m💡 On Windows: git pull && just use\033[0m\n"