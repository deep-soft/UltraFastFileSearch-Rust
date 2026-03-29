<#
.SYNOPSIS
    Diagnostic: check UFFS cache status, run searches, observe cache behavior.
    Set UFFS_CACHE_PROFILE=1 automatically to show per-phase timing breakdown.
.PARAMETER Drive
    Drive letter to test (default: G)
.PARAMETER Rounds
    Number of search runs (default: 3)
.PARAMETER NoProfile
    Disable per-phase profiling output (default: profiling is ON)
#>
param(
    [string]$Drive = "G",
    [int]$Rounds = 3,
    [switch]$NoProfile
)

$ErrorActionPreference = "Stop"
$UFFS = "$env:USERPROFILE\bin\uffs.exe"
$CacheDir = "$env:LOCALAPPDATA\uffs\cache"
$CacheDirLegacy = "$env:TEMP\uffs_index_cache"
$CacheFile = Join-Path $CacheDir "${Drive}_index.uffs"

# Enable cache profiling unless suppressed
if (-not $NoProfile) {
    $env:UFFS_CACHE_PROFILE = "1"
}

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

# Show binary version
$uffsVersion = & $UFFS --version 2>&1 | Select-Object -First 1
Write-Host "  Binary: $UFFS" -ForegroundColor Gray
Write-Host "  Version: $uffsVersion" -ForegroundColor Gray

# 1. Clear cache for this drive
Write-Host "`n[STEP 1] Clearing cache for drive ${Drive}:..." -ForegroundColor Yellow
if (Test-Path $CacheFile) {
    Remove-Item $CacheFile -Force
    Write-Host "  Removed $CacheFile" -ForegroundColor DarkGray
}
$compactCacheFile = Join-Path $CacheDir "${Drive}_compact.uffs"
if (Test-Path $compactCacheFile) {
    Remove-Item $compactCacheFile -Force
    Write-Host "  Removed $compactCacheFile" -ForegroundColor DarkGray
}
$legacyCacheFile = Join-Path $CacheDirLegacy "${Drive}_index.uffs"
if (Test-Path $legacyCacheFile) {
    Remove-Item $legacyCacheFile -Force
    Write-Host "  Removed legacy $legacyCacheFile" -ForegroundColor DarkGray
}

# 2. Show cache state after clearing
Write-Host "`n[STEP 2] Cache state (post-clear):" -ForegroundColor Yellow
Show-CacheStatus

# 3. Run search N times, show timing and cache status after each
Write-Host "`n[STEP 3] Running $Rounds searches (uffs `"*`" --drive $Drive):" -ForegroundColor Yellow
Write-Host ""

$runTimings = @()

# Helper: run uffs and capture profile lines + row count from stderr.
# Stdout is discarded (sent to NUL) — counting 7M lines in PowerShell
# added 2-3 MINUTES of overhead per run.  The row count is already in
# the CACHE_PROFILE output: "row_output: 170 ms (7065511 rows)".
function Invoke-UffsProfiled {
    param([string]$ArgString)
    $stderrFile = [System.IO.Path]::GetTempFileName()
    $sw = [System.Diagnostics.Stopwatch]::StartNew()
    $proc = Start-Process -FilePath $UFFS -ArgumentList $ArgString `
        -RedirectStandardOutput NUL -RedirectStandardError $stderrFile `
        -NoNewWindow -PassThru -Wait
    $sw.Stop()
    $ms = [math]::Round($sw.Elapsed.TotalMilliseconds)
    $profileLines = @()
    $lineCount = 0
    if (Test-Path $stderrFile) {
        $stderrContent = Get-Content $stderrFile -ErrorAction SilentlyContinue
        $profileLines = @($stderrContent | Where-Object { $_ -match '^\[CACHE_PROFILE\]' })
        # Extract row count from profile: "row_output: ... (12345 rows)"
        $rowLine = $stderrContent | Where-Object { $_ -match 'row_output:.*\((\d+) rows?\)' } | Select-Object -First 1
        if ($rowLine -match '\((\d+) rows?\)') { $lineCount = [int64]$Matches[1] }
    }
    Remove-Item $stderrFile -Force -ErrorAction SilentlyContinue
    [PSCustomObject]@{ Ms = $ms; Lines = $lineCount; Profile = $profileLines }
}

1..$Rounds | ForEach-Object {
    $runNum = $_
    $label = if ($runNum -eq 1) { "COLD (no cache)" } else { "RUN $runNum (should use cache)" }
    Write-Host "  ── Run $runNum ($label) ──" -ForegroundColor Cyan

    $result = Invoke-UffsProfiled "`"*`" --drive $Drive"

    $linesLabel = if ($result.Lines -gt 0) { "$($result.Lines) rows" } else { "rows N/A" }
    Write-Host "     Time: $($result.Ms) ms ($linesLabel)" -ForegroundColor $(if ($result.Ms -lt 2000) { "Green" } else { "White" })

    # Display profile lines from stderr
    if ($result.Profile.Count -gt 0) {
        foreach ($line in $result.Profile) {
            $display = $line -replace '^\[CACHE_PROFILE\]\s*', ''
            Write-Host "     ⏱  $display" -ForegroundColor DarkCyan
        }
    }

    # Check cache file after this run
    if (Test-Path $CacheFile) {
        $info = Get-Item $CacheFile
        $age = [math]::Round(((Get-Date) - $info.LastWriteTime).TotalSeconds)
        $sizeMB = [math]::Round($info.Length / 1MB, 2)
        Write-Host "     Cache: $sizeMB MB, age=${age}s ✅" -ForegroundColor Green
    } else {
        Write-Host "     Cache: NOT FOUND ❌" -ForegroundColor Red
    }

    $runTimings += @{ Run = $runNum; Label = $label; Ms = $result.Ms; Lines = $result.Lines }
    Write-Host ""
}

# 4. Final cache status
Write-Host "[STEP 4] Final cache state:" -ForegroundColor Yellow
Show-CacheStatus

# 5. Test --no-cache flag
Write-Host "[STEP 5] Running with --no-cache (should bypass cache):" -ForegroundColor Yellow
$noCacheResult = Invoke-UffsProfiled "`"*`" --drive $Drive --no-cache"
$ms = $noCacheResult.Ms
$ncLabel = if ($noCacheResult.Lines -gt 0) { "$($noCacheResult.Lines) rows" } else { "rows N/A" }
Write-Host "     Time: ${ms} ms ($ncLabel)" -ForegroundColor White
if ($noCacheResult.Profile.Count -gt 0) {
    foreach ($line in $noCacheResult.Profile) {
        $display = $line -replace '^\[CACHE_PROFILE\]\s*', ''
        Write-Host "     ⏱  $display" -ForegroundColor DarkCyan
    }
}

# 6. Summary
Write-Host "`n─── Summary ───" -ForegroundColor Cyan
$coldMs  = if ($runTimings.Count -gt 0) { $runTimings[0].Ms } else { 0 }
$cachedAvg = if ($runTimings.Count -gt 1) {
    [math]::Round(($runTimings[1..($runTimings.Count - 1)] | ForEach-Object { $_.Ms } |
        Measure-Object -Average).Average)
} else { 0 }
$speedup = if ($cachedAvg -gt 0) { [math]::Round($coldMs / $cachedAvg, 1) } else { 0 }

Write-Host "  Cold (Run 1):    $coldMs ms" -ForegroundColor White
Write-Host "  Cached (avg):    $cachedAvg ms" -ForegroundColor $(if ($cachedAvg -lt $coldMs) { "Green" } else { "Yellow" })
Write-Host "  No-cache:        $ms ms" -ForegroundColor White
Write-Host "  Speedup:         ${speedup}x (cache vs cold)" -ForegroundColor $(if ($speedup -gt 2) { "Green" } else { "Yellow" })

if ($speedup -lt 1.5) {
    Write-Host "`n⚠️  Cache is NOT providing significant speedup." -ForegroundColor Red
} elseif ($speedup -lt 3) {
    Write-Host "`n✅ Cache is working. Bottleneck is likely deserialization or output." -ForegroundColor Yellow
} else {
    Write-Host "`n✅ Cache is working well!" -ForegroundColor Green
}

# Cleanup env var
if (-not $NoProfile) {
    Remove-Item Env:\UFFS_CACHE_PROFILE -ErrorAction SilentlyContinue
}

