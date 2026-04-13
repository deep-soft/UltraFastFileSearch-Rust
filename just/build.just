# SPDX-License-Identifier: MPL-2.0
# Copyright (c) 2025-2026 SKY, LLC.

# Show current version.
version:
    @printf "\033[0;34m📋 Current version:\033[0m\n"
    @cargo metadata --format-version 1 | jq -r '.packages[] | select(.name == "uffs-cli") | .version'

# Increment version.
version-bump:
    @printf "\033[0;34m📈 Incrementing version...\033[0m\n"
    @if [ -f "./build/update_all_versions.rs" ]; then \
        ./build/update_all_versions.rs patch; \
    else \
        printf "\033[1;33mVersion script not found, using manual increment...\033[0m\n"; \
        echo "Manual version increment needed"; \
    fi

# Build dev binary.
build:
    @printf "\033[0;34m🔨 Building dev binary (FAST-FAIL)...\033[0m\n"
    cargo build --workspace

# Build release binary.
build-release:
    @printf "\033[0;34m🔨 Building optimized release binary...\033[0m\n"
    @printf "\033[1;33m⚠️  This will take significantly longer than dev builds\033[0m\n"
    cargo build --release --workspace

# Build and install all binaries locally.
build-local:
    @printf "\033[0;34m🚀 Building and installing all UFFS binaries locally...\033[0m\n"
    @printf "\033[1;33m📦 Platform-specific build with Fat LTO optimization\033[0m\n"
    @printf "\033[1;33m📁 Installing to ~/bin with PATH setup\033[0m\n"
    @echo "========================================================"
    rust-script scripts/dev/build-local.rs

# Quick alias for local build and install.
install: build-local

# Deploy binary to ~/bin.
deploy:
    @printf "\033[0;34m📦 Deploying dev binary...\033[0m\n"
    just copy-binary debug

# Deploy release binary to ~/bin.
deploy-release:
    @printf "\033[0;34m📦 Deploying release binary...\033[0m\n"
    just copy-binary release

# Cross-platform binary deployment with intelligent path handling.
copy-binary profile:
    #!/usr/bin/env bash
    printf "\033[0;34m📦 Cross-platform binary deployment ({{ profile }})...\033[0m\n"
    if [[ "$OSTYPE" == "msys"* ]] || [[ "$OSTYPE" == "cygwin"* ]] || [[ -n "$WINDIR" ]]; then TARGET_BINARY="uffs.exe"; printf "\033[0;34m  → Windows detected\033[0m\n"; else TARGET_BINARY="uffs"; printf "\033[0;34m  → Unix-like system detected\033[0m\n"; fi
    SOURCE_DIR="target/{{ profile }}"
    SOURCE_PATH="$SOURCE_DIR/$TARGET_BINARY"
    if [[ ! -f "$SOURCE_PATH" ]]; then printf "\033[0;31m❌ Binary not found: $SOURCE_PATH\033[0m\n"; printf "\033[1;33m💡 Run 'just build' first\033[0m\n"; exit 1; fi
    mkdir -p ~/bin
    DEST_PATH="$HOME/bin/$TARGET_BINARY"
    printf "\033[0;34m  → Source: $SOURCE_PATH\033[0m\n"
    printf "\033[0;34m  → Destination: $DEST_PATH\033[0m\n"
    if cp "$SOURCE_PATH" "$DEST_PATH" && chmod +x "$DEST_PATH"; then
        printf "\033[0;32m✅ Binary deployed to ~/bin/$TARGET_BINARY\033[0m\n"
    else
        printf "\033[0;31m❌ Failed to copy binary\033[0m\n"
        exit 1
    fi
    if ! echo "$PATH" | grep -q "$HOME/bin"; then printf "\033[1;33m⚠️  ~/bin not in PATH. Add this to your shell profile:\033[0m\n"; printf "   export PATH=\"\$HOME/bin:\$PATH\"\n"; fi

# Build release binaries and install all platform-relevant ones to ~/bin.
#
# Platform binaries:
#   All platforms : uffs, uffs-daemon, uffs-mcp
#   Windows only  : uffs-broker
#   macOS/Linux   : (uffs-broker excluded — Windows-only component)
#
# NOTE: uffs_tui and uffs_gui have moved to the private uffs-products repo.
[unix]
use:
    #!/usr/bin/env bash
    set -euo pipefail
    printf "\033[0;34m🔨 Building release binaries...\033[0m\n"
    cargo build --release --workspace 2>&1 | tail -5

    BIN_DIR="$HOME/bin"
    mkdir -p "$BIN_DIR"

    # Platform-relevant binaries (name in target/release → name in ~/bin)
    BINARIES=("uffs" "uffs-daemon" "uffs-mcp")
    INSTALLED=0
    SKIPPED=0

    printf "\033[0;34m📦 Installing to %s\033[0m\n" "$BIN_DIR"
    for BIN in "${BINARIES[@]}"; do
        SRC="target/release/$BIN"
        if [[ -f "$SRC" ]]; then
            cp "$SRC" "$BIN_DIR/$BIN"
            chmod +x "$BIN_DIR/$BIN"
            SIZE=$(du -sh "$BIN_DIR/$BIN" | cut -f1)
            printf "  \033[0;32m✅ %-16s %s\033[0m\n" "$BIN" "$SIZE"
            INSTALLED=$((INSTALLED + 1))
        else
            printf "  \033[1;33m⚠️  %-16s (not found in target/release/)\033[0m\n" "$BIN"
            SKIPPED=$((SKIPPED + 1))
        fi
    done

    printf "\n\033[0;32m✅ Installed %d binaries to ~/bin\033[0m\n" "$INSTALLED"
    if [[ $SKIPPED -gt 0 ]]; then
        printf "\033[1;33m⚠️  Skipped %d (build may have failed)\033[0m\n" "$SKIPPED"
    fi

    # Check PATH
    if ! echo "$PATH" | grep -q "$HOME/bin"; then
        printf "\n\033[1;33m⚠️  ~/bin not in PATH. Add to your shell profile:\033[0m\n"
        printf "   export PATH=\"\$HOME/bin:\$PATH\"\n"
    fi

    # Show versions
    printf "\n\033[0;34m📋 Installed versions:\033[0m\n"
    for BIN in "${BINARIES[@]}"; do
        if [[ -x "$BIN_DIR/$BIN" ]]; then
            VER=$("$BIN_DIR/$BIN" --version 2>/dev/null || echo "unknown")
            printf "  %-16s %s\n" "$BIN" "$VER"
        fi
    done

# Install UFFS binaries from the latest GitHub Release to ~/bin (Windows).
# Downloads pre-built binaries using `gh release download`.
# Prerequisite: `gh` CLI installed and authenticated (winget install GitHub.cli)
[windows]
use:
    #!powershell
    Write-Host "📦 Downloading latest UFFS binaries from GitHub Release..." -ForegroundColor Blue

    # Check gh CLI is available
    if (-not (Get-Command gh -ErrorAction SilentlyContinue)) {
        Write-Host "❌ gh CLI not found. Install with: winget install GitHub.cli" -ForegroundColor Red
        exit 1
    }

    $binDir = "$env:USERPROFILE\bin"
    if (-not (Test-Path $binDir)) { New-Item -ItemType Directory -Path $binDir -Force | Out-Null }

    # Create temp download directory
    $tempDir = Join-Path $env:TEMP "uffs-release-$(Get-Date -Format 'yyyyMMddHHmmss')"
    New-Item -ItemType Directory -Path $tempDir -Force | Out-Null

    # Download latest release assets matching windows pattern
    Write-Host "  → Downloading from latest release..." -ForegroundColor Cyan
    gh release download --latest --pattern "*windows*" --dir $tempDir 2>&1
    if ($LASTEXITCODE -ne 0) {
        Write-Host "❌ Failed to download release. Run 'gh auth login' if not authenticated." -ForegroundColor Red
        Remove-Item $tempDir -Recurse -Force -ErrorAction SilentlyContinue
        exit 1
    }

    # Install downloaded binaries
    $binaries = @{
        "uffs-windows-x64.exe" = "uffs.exe"
        "uffs_mft-windows-x64.exe" = "uffs_mft.exe"
    }
    $installed = 0; $skipped = 0
    foreach ($src in $binaries.Keys) {
        $srcPath = Join-Path $tempDir $src
        $destPath = Join-Path $binDir $binaries[$src]
        if (Test-Path $srcPath) {
            Copy-Item $srcPath $destPath -Force
            $size = [math]::Round((Get-Item $destPath).Length / 1MB, 1)
            Write-Host "  ✅ $($binaries[$src]) ($size MB)" -ForegroundColor Green
            $installed++
        } else {
            Write-Host "  ⚠️  $src (not in release)" -ForegroundColor Yellow
            $skipped++
        }
    }

    # Clean up
    Remove-Item $tempDir -Recurse -Force -ErrorAction SilentlyContinue

    Write-Host "`n✅ Installed $installed binaries to ~/bin" -ForegroundColor Green
    if ($skipped -gt 0) { Write-Host "⚠️  Skipped $skipped" -ForegroundColor Yellow }

    # Show versions
    Write-Host "`n📋 Installed versions:" -ForegroundColor Blue
    foreach ($dest in $binaries.Values) {
        $destPath = Join-Path $binDir $dest
        if (Test-Path $destPath) {
            $ver = & $destPath --version 2>$null
            Write-Host "  $dest`t$ver"
        }
    }

# Git commit with auto-generated message.
commit:
    @printf "\033[0;34m📝 Creating auto-generated commit...\033[0m\n"
    git add .
    @NEW_VERSION=`awk '/^\[workspace\.package\]/{flag=1; next} flag && /^version/{print $3; exit}' Cargo.toml | tr -d '"'`; \
    git -c commit.gpgsign=false commit -m "chore: release v$NEW_VERSION - comprehensive testing complete [auto-commit]"

# Git push to remote.
push:
    @printf "\033[0;34m🚀 Pushing to remote...\033[0m\n"
    @if git diff --quiet && git diff --cached --quiet; then echo "📋 Working directory is clean"; else echo "📋 Committing pending changes..."; git add .; git -c commit.gpgsign=false commit -m "chore: commit pending changes before push [auto-commit]"; fi
    git pull origin main --rebase
    git push origin main

