<#
.SYNOPSIS
    Diagnostic: check UFFS cache status, run searches, observe cache behavior.
.PARAMETER Drive
    Drive letter to test (default: G)
.PARAMETER Rounds
    Number of search runs (default: 3)
#>
param(
    [string]$Drive = "G",
    [int]$Rounds = 3
)

$ErrorActionPreference = "Stop"
$UFFS = "$env:USERPROFILE\bin\uffs.exe"
$CacheDir = "$env:LOCALAPPDATA\uffs\cache"
$CacheDirLegacy = "$env:TEMP\uffs_index_cache"
$CacheFile = Join-Path $CacheDir "${Drive}_index.uffs"

function Show-CacheStatus {
    Write-Host "`n─── Cache Status ───" -ForegroundColor Cyan
    Write-Host "  Secure dir:  $CacheDir" -ForegroundColor Gray
    Write-Host "  Legacy dir:  $CacheDirLegacy" -ForegroundColor Gray
    Write-Host "  Cache file:  $CacheFile" -ForegroundColor Gray

    if (Test-Path $CacheDir) {
        $files = Get-ChildItem $CacheDir -Filter "*.uffs" -ErrorAction SilentlyContinue
        if ($files) {
            foreach ($f in $files) {
                $age = [math]::Round(((Get-Date) - $f.LastWriteTime).TotalSeconds)
                $sizeMB = [math]::Round($f.Length / 1MB, 2)
                $fresh = if ($age -lt 600) { "✅ FRESH" } else { "⏰ STALE" }
                Write-Host "    $($f.Name): $sizeMB MB, age=${age}s $fresh" -ForegroundColor $(if ($age -lt 600) { "Green" } else { "Yellow" })
            }
        } else {
            Write-Host "    (no .uffs files)" -ForegroundColor DarkYellow
        }
    } else {
        Write-Host "    (cache dir does not exist)" -ForegroundColor Red
    }

    if (Test-Path $CacheDirLegacy) {
        $legacyFiles = Get-ChildItem $CacheDirLegacy -ErrorAction SilentlyContinue
        if ($legacyFiles) {
            Write-Host "  ⚠️  Legacy cache still has files:" -ForegroundColor Yellow
            foreach ($f in $legacyFiles) {
                Write-Host "    $($f.Name): $([math]::Round($f.Length / 1MB, 2)) MB" -ForegroundColor Yellow
            }
        }
    }
    Write-Host ""
}

Write-Host "╔════════════════════════════════════════╗" -ForegroundColor Cyan
Write-Host "║    UFFS Cache Diagnostic — Drive $Drive     ║" -ForegroundColor Cyan
Write-Host "╚════════════════════════════════════════╝" -ForegroundColor Cyan

# 1. Show initial cache status
Write-Host "`n[STEP 1] Initial cache state:" -ForegroundColor Yellow
Show-CacheStatus

# 2. Clear cache for this drive
Write-Host "[STEP 2] Clearing cache for drive ${Drive}:..." -ForegroundColor Yellow
if (Test-Path $CacheFile) {
    Remove-Item $CacheFile -Force
    Write-Host "  Removed $CacheFile" -ForegroundColor DarkGray
}
$legacyCacheFile = Join-Path $CacheDirLegacy "${Drive}_index.uffs"
if (Test-Path $legacyCacheFile) {
    Remove-Item $legacyCacheFile -Force
    Write-Host "  Removed legacy $legacyCacheFile" -ForegroundColor DarkGray
}
Show-CacheStatus

# 3. Run search N times, show timing and cache status after each
Write-Host "[STEP 3] Running $Rounds searches (uffs `"*`" --drive $Drive):" -ForegroundColor Yellow
Write-Host ""

1..$Rounds | ForEach-Object {
    $label = if ($_ -eq 1) { "COLD (no cache)" } else { "RUN $_ (should use cache)" }
    Write-Host "  ── Run $_ ($label) ──" -ForegroundColor Cyan

    $sw = [System.Diagnostics.Stopwatch]::StartNew()
    $lineCount = (& $UFFS "*" --drive $Drive 2>$null | Measure-Object -Line).Lines
    $sw.Stop()
    $ms = [math]::Round($sw.Elapsed.TotalMilliseconds)

    Write-Host "     Time: ${ms} ms ($lineCount lines)" -ForegroundColor $(if ($ms -lt 2000) { "Green" } else { "White" })

    # Check cache file after this run
    if (Test-Path $CacheFile) {
        $info = Get-Item $CacheFile
        $age = [math]::Round(((Get-Date) - $info.LastWriteTime).TotalSeconds)
        $sizeMB = [math]::Round($info.Length / 1MB, 2)
        Write-Host "     Cache: $sizeMB MB, age=${age}s ✅" -ForegroundColor Green
    } else {
        Write-Host "     Cache: NOT FOUND ❌" -ForegroundColor Red
    }
    Write-Host ""
}

# 4. Final cache status
Write-Host "[STEP 4] Final cache state:" -ForegroundColor Yellow
Show-CacheStatus

# 5. Test --no-cache flag
Write-Host "[STEP 5] Running with --no-cache (should bypass cache):" -ForegroundColor Yellow
$sw = [System.Diagnostics.Stopwatch]::StartNew()
$lineCount = (& $UFFS "*" --drive $Drive --no-cache 2>$null | Measure-Object -Line).Lines
$sw.Stop()
$ms = [math]::Round($sw.Elapsed.TotalMilliseconds)
Write-Host "     Time: ${ms} ms ($lineCount lines)" -ForegroundColor White
Write-Host "     (Should be similar to Run 1 cold time)" -ForegroundColor DarkGray

Write-Host "`n✅ Done. If Run 2+ times are similar to Run 1, cache is NOT working." -ForegroundColor Yellow
Write-Host "   If Run 2+ are ~2-3x faster, cache IS working." -ForegroundColor Yellow

