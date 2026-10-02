#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2025-2026 SKY, LLC.
# SPDX-License-Identifier: MPL-2.0
#
# Idempotently seed every "extra" PortableCommandAlias into a WinGet
# `SkyLLC.UFFS.installer.yaml` manifest.
#
# WHY THIS EXISTS
# ---------------
# `winget-publish.yml` auto-submits the SkyLLC.UFFS manifest via
# winget-releaser (komac). komac PRESERVES the previous version's
# NestedInstallerFiles / PortableCommandAlias list across updates, but it
# does NOT auto-add new executables it finds in the zip. So each binary we
# bundle beyond the founding four (uffs / uffsd / uffsmcp / uffs-mft) must
# be seeded into the manifest exactly ONCE; thereafter komac carries it
# forward on every auto-submitted release.
#
# The canonical list of those extra aliases lives in
# `packaging/winget/nested-aliases.yaml` — this script reads it and inserts
# any entry not already present. Adding a future binary is a one-line edit
# to that file, never a change here.
#
# USAGE
# -----
#   scripts/dev/winget_seed_aliases.sh <path/to/SkyLLC.UFFS.installer.yaml>
#
# Typical flow (per binary, one time):
#   1. winget-releaser opens a PR to microsoft/winget-pkgs from the
#      githubrobbi/winget-pkgs fork bumping SkyLLC.UFFS.
#   2. Check out that PR branch on the fork.
#   3. Run this script against the new version's installer manifest:
#        scripts/dev/winget_seed_aliases.sh \
#          manifests/s/SkyLLC/UFFS/0.5.XXX/SkyLLC.UFFS.installer.yaml
#   4. Commit + push to the PR branch; let validation pass; merge.
#   5. Verify:  winget install --id SkyLLC.UFFS  &&  uffs-broker --help
#
# Safe to re-run: aliases already present are skipped; exits 0 with no
# changes when everything is already seeded.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ALIASES_FILE="${SCRIPT_DIR}/../../packaging/winget/nested-aliases.yaml"

MANIFEST="${1:-}"
if [[ -z "$MANIFEST" ]]; then
  echo "Usage: $0 <path/to/SkyLLC.UFFS.installer.yaml>" >&2
  exit 2
fi
if [[ ! -f "$MANIFEST" ]]; then
  echo "❌ Manifest not found: $MANIFEST" >&2
  exit 1
fi
if [[ ! -f "$ALIASES_FILE" ]]; then
  echo "❌ Canonical alias list not found: $ALIASES_FILE" >&2
  exit 1
fi

# Sanity: this must be the SkyLLC.UFFS portable-in-zip installer manifest.
if ! grep -q '^NestedInstallerFiles:' "$MANIFEST"; then
  echo "❌ $MANIFEST has no 'NestedInstallerFiles:' section — wrong file?" >&2
  exit 1
fi
if ! grep -q 'PackageIdentifier: SkyLLC.UFFS' "$MANIFEST"; then
  echo "❌ $MANIFEST is not the SkyLLC.UFFS manifest." >&2
  exit 1
fi

# winget-pkgs manifests use CRLF; mixing LF trips the pipeline's
# "Validation-Line-Endings-Error". Match the manifest's terminator.
cr=''
if grep -q $'\r$' "$MANIFEST"; then
  cr=$'\r'
fi

# Read the actual zip contents so we can enforce the HARD PRECONDITION that
# `packaging/winget/nested-aliases.yaml` states: only an entry whose binary is
# really in the archive may be seeded.
#
# This is not hypothetical. The bundled demo TUI is fetched from a separate
# repository by `release.yml`, and that fetch is deliberately non-fatal — a
# demo-repo hiccup must never block an engine release. When the upstream asset
# was renamed (uffs-products 2997eee7, 2026-09-07) the download began missing
# silently, v0.6.41 shipped a zip with no `uffs-tui.exe`, and this script
# seeded the alias anyway. Microsoft's validation then rejected the PR for
# declaring a file that did not exist (microsoft/winget-pkgs#437613).
#
# Degradable on purpose: if the archive cannot be fetched or listed, we warn
# and fall back to the previous behaviour rather than blocking a submission on
# a network hiccup.
zip_listing=''
installer_url="$(sed -n 's/^[[:space:]]*InstallerUrl:[[:space:]]*//p' "$MANIFEST" | head -1 | tr -d '\r')"
if [[ -n "$installer_url" ]] && command -v unzip >/dev/null 2>&1; then
  zip_tmp="$(mktemp -d)"
  trap 'rm -rf "$zip_tmp"' EXIT
  if curl -fsSL "$installer_url" -o "$zip_tmp/installer.zip" 2>/dev/null \
     && zip_listing="$(unzip -Z1 "$zip_tmp/installer.zip" 2>/dev/null)"; then
    echo "🔍 Verifying aliases against $(printf '%s' "$zip_listing" | wc -l | tr -d ' ') archive entries."
  else
    zip_listing=''
    echo "::warning title=Zip verification skipped::Could not fetch or list ${installer_url}; seeding without archive verification."
  fi
fi

# Parse (RelativeFilePath, PortableCommandAlias) pairs from the canonical
# list. The two keys always appear on consecutive lines per entry.
added=0
absent=0
skipped=0
rel=''
while IFS= read -r line; do
  line="${line%$'\r'}"
  case "$line" in
    *RelativeFilePath:*)
      rel="$(printf '%s' "$line" | sed -E 's/.*RelativeFilePath:[[:space:]]*//')"
      ;;
    *PortableCommandAlias:*)
      alias="$(printf '%s' "$line" | sed -E 's/.*PortableCommandAlias:[[:space:]]*//')"
      [[ -z "$rel" || -z "$alias" ]] && continue

      # Not in the archive? Never declare it — winget validates every
      # NestedInstallerFile against the zip and fails the whole PR.
      if [[ -n "$zip_listing" ]] && ! printf '%s\n' "$zip_listing" | grep -qxF "$rel"; then
        echo "⚠️  ${alias} (${rel}) is NOT in the archive — skipping (would fail validation)."
        absent=$((absent + 1))
        rel=''
        continue
      fi

      # Already seeded? (match on the relative path, which is unique).
      if grep -qF "$rel" "$MANIFEST"; then
        echo "✅ ${alias} already present — skipping."
        skipped=$((skipped + 1))
        rel=''
        continue
      fi

      # Insert the two-line nested-file block right after the
      # `NestedInstallerFiles:` key so it joins the existing alias list.
      tmp="$(mktemp)"
      awk -v cr="$cr" -v rel="$rel" -v alias="$alias" '
        /^NestedInstallerFiles:[[:space:]]*$/ {
          print
          printf "%s%s\n", "- RelativeFilePath: " rel, cr
          printf "%s%s\n", "  PortableCommandAlias: " alias, cr
          next
        }
        { print }
      ' "$MANIFEST" > "$tmp"
      mv "$tmp" "$MANIFEST"
      echo "➕ Added ${alias} (${rel})"
      added=$((added + 1))
      rel=''
      ;;
  esac
done < "$ALIASES_FILE"

# Strip the no-op top-level `Scope:` field. It carried over from the
# founding manifest template, but a zip/portable installer has no
# per-user/per-machine scope — winget's validator emits "Scope is not
# supported for InstallerType portable" on every version. Removing it
# clears the warning, and komac preserves the absence on the next
# version bump. Idempotent: a no-op once it's gone.
scope_removed=0
if grep -qE '^Scope:' "$MANIFEST"; then
  tmp="$(mktemp)"
  grep -vE '^Scope:' "$MANIFEST" > "$tmp"
  mv "$tmp" "$MANIFEST"
  echo "➖ Removed unsupported 'Scope' field (portable installer)"
  scope_removed=1
fi

echo
echo "Done: ${added} added, ${skipped} already present, ${absent} absent from archive, ${scope_removed} scope removed in $MANIFEST"
if (( absent > 0 )); then
  echo "::warning title=Alias skipped::${absent} canonical alias(es) are missing from the release archive — check the bundling step in release.yml."
fi
if (( added > 0 || scope_removed > 0 )); then
  echo "Review the diff, commit, and push to the winget-pkgs PR branch."
fi
