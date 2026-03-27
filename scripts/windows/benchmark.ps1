# UFFS Benchmark (unified cold / cached)
# Default: cold start (cache cleared before EACH run)
# With -Cache: warm start (cache persists across runs)
#
# Usage:
#   .\benchmark.ps1 -N 5 -Drive C,D,E,F,G,M,S                    # cold full scan
#   .\benchmark.ps1 -N 5 -Drive C,D,E,F,G,M,S -Cache              # warm full scan
#   .\benchmark.ps1 -N 5 -Drive C,F -Cache -Pattern "*.rs"         # warm filtered
#   .\benchmark.ps1 -N 5 -Drive C -Cache -RustArgs "--files-only --min-size 1024"
#   .\benchmark.ps1 -N 5 -Drive C -Cache -RustArgs "--sort size:desc --limit 100"
#   .\benchmark.ps1 -N 5 -Drive C -Cache -RustArgs "--attr hidden,!system --newer 7d"

param(
    [int]$N = 3,                    # Rounds per test
    [string]$Pattern = "*",         # Search pattern (default: "*" for everything)
    [string[]]$Drive = @(),         # Drives (comma-separated): -Drive C,D,E,F
    [switch]$Cache,                 # Keep cache between runs (warm benchmark)
    [switch]$RustOnly,              # Skip C++ and Everything tests
    [switch]$CppOnly,               # Skip Rust and Everything tests
    [switch]$EverythingOnly,        # Skip C++ and Rust tests
    [switch]$NoAll,                 # Skip the final "all drives" parallel run
    [switch]$NoEverything,          # Skip Everything benchmark
    [string]$RustArgs = "",         # Extra args for Rust (e.g. "--files-only --min-size 1024")
    [string]$CppArgs = ""           # Extra args for C++ (e.g. "--limit=100")
)

$ErrorActionPreference = "Stop"
$UFFS = "$env:USERPROFILE\bin\uffs.exe"
$UFFS_CPP = "$env:USERPROFILE\bin\uffs.com"
$CACHE_DIR = "$env:TEMP\uffs_index_cache"

# Everything detection
$pf86 = ${env:ProgramFiles(x86)}
$EVERYTHING_EXE = $null
$ES_EXE = $null
$EVERYTHING_INI = Join-Path $env:APPDATA "Everything\Everything.ini"

foreach ($p in @(
    (Join-Path ${env:ProgramFiles} "Everything\Everything.exe"),
    $(if ($pf86) { Join-Path $pf86 "Everything\Everything.exe" }),
    (Join-Path "$env:USERPROFILE\bin" "Everything.exe")
)) { if ($p -and (Test-Path -LiteralPath $p)) { $EVERYTHING_EXE = $p; break } }

foreach ($p in @(
    (Join-Path "$env:USERPROFILE\bin" "es.exe"),
    (Join-Path ${env:ProgramFiles} "Everything\es.exe"),
    $(if ($pf86) { Join-Path $pf86 "Everything\es.exe" })
)) { if ($p -and (Test-Path -LiteralPath $p)) { $ES_EXE = $p; break } }

$hasEverything = $EVERYTHING_EXE -and $ES_EXE -and (Test-Path -LiteralPath $EVERYTHING_INI)

# Normalize drives to uppercase
$AllDrives = $Drive | ForEach-Object { $_.ToUpper().Trim() } | Where-Object { $_ }

$mode = if ($Cache) { "Cached (warm)" } else { "Cold Start" }

Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "  UFFS Benchmark — $mode" -ForegroundColor Cyan
Write-Host "  Rounds per test: $N" -ForegroundColor Cyan
Write-Host "  Pattern: $Pattern" -ForegroundColor Cyan
if ($AllDrives.Count -gt 0) {
    Write-Host "  Drives: $($AllDrives -join ', ')" -ForegroundColor Cyan
}
if ($RustArgs) {
    Write-Host "  Rust args: $RustArgs" -ForegroundColor Cyan
}
if ($CppArgs) {
    Write-Host "  C++ args: $CppArgs" -ForegroundColor Cyan
}
Write-Host "  Rust:       $(if (Test-Path $UFFS) { '✅' } else { '❌' }) $UFFS" -ForegroundColor Cyan
Write-Host "  C++:        $(if (Test-Path $UFFS_CPP) { '✅' } else { '❌' }) $UFFS_CPP" -ForegroundColor Cyan
Write-Host "  Everything: $(if ($hasEverything) { '✅' } else { '❌' }) $(if ($EVERYTHING_EXE) { $EVERYTHING_EXE } else { '(not found)' })" -ForegroundColor Cyan
if ($Cache) {
    Write-Host "  (Cache kept between runs)" -ForegroundColor Cyan
} else {
    Write-Host "  (Cache cleared before EACH run)" -ForegroundColor Cyan
}
Write-Host "========================================`n" -ForegroundColor Cyan

# Show cache status when running in cached mode
if ($Cache) {
    if (Test-Path $CACHE_DIR) {
        $cacheFiles = Get-ChildItem $CACHE_DIR -Filter "*.uffs" -ErrorAction SilentlyContinue
        Write-Host "📦 Cache status: $($cacheFiles.Count) cached drive(s)" -ForegroundColor Gray
        foreach ($f in $cacheFiles) {
            $age = [math]::Round(((Get-Date) - $f.LastWriteTime).TotalMinutes, 1)
            Write-Host "   - $($f.Name) (age: ${age}m, size: $([math]::Round($f.Length/1MB, 1))MB)" -ForegroundColor Gray
        }
        Write-Host ""
    } else {
        Write-Host "📦 Cache status: Empty (first run will populate)`n" -ForegroundColor Gray
    }
}

function BenchRun($label, $exePath, [string[]]$argList) {
    Write-Host "▶ $label" -ForegroundColor Yellow
    $times = @()
    1..$N | ForEach-Object {
        # Clear cache before each run in cold mode
        if (-not $Cache) {
            Remove-Item $CACHE_DIR -Recurse -Force -ErrorAction SilentlyContinue
        }

        # Redirect stdout to NUL — we only need wall-clock time and any
        # [TIMING]/[DIAG] lines that go to stderr.  Writing millions of
        # result lines to a temp file added 10-20s of pure I/O overhead per
        # run, and the subsequent Select-String scan of that multi-GB file
        # added another 5-10s.
        $tempErr = [System.IO.Path]::GetTempFileName()

        # Show exact command on first run only
        if ($_ -eq 1) {
            Write-Host "     CMD: $exePath $($argList -join ' ')" -ForegroundColor DarkGray
        }
        $sw = [System.Diagnostics.Stopwatch]::StartNew()
        try {
            $proc = Start-Process -FilePath $exePath -ArgumentList $argList `
                -RedirectStandardOutput "NUL" -RedirectStandardError $tempErr `
                -NoNewWindow -Wait -PassThru
        } catch {
            Write-Host "   ⚠️  Error: $_" -ForegroundColor Red
        }
        $sw.Stop()
        $ms = $sw.Elapsed.TotalMilliseconds
        $times += $ms
        Write-Host "   Run $_`: $([math]::Round($ms/1000, 2))s" -ForegroundColor Gray

        # Extract [TIMING] and [DIAG] lines from stderr (small file)
        try {
            $timingLines = Select-String -Path $tempErr -Pattern '\[TIMING\]|\[DIAG\]' | ForEach-Object { $_.Line }
            if ($timingLines) {
                foreach ($line in $timingLines) {
                    Write-Host "     $line" -ForegroundColor DarkCyan
                }
            }
        } catch {
            # Ignore errors reading temp file
        }

        # Clean up temp file
        Remove-Item $tempErr -Force -ErrorAction SilentlyContinue
    }

    if ($times.Count -gt 0) {
        $avg = ($times | Measure-Object -Average).Average
        $min = ($times | Measure-Object -Minimum).Minimum
        $max = ($times | Measure-Object -Maximum).Maximum
        Write-Host ("{0,-20} avg={1,8:N0} ms   min={2,8:N0}   max={3,8:N0}" -f $label, $avg, $min, $max) -ForegroundColor Green
    }
    Write-Host ""
}

function BenchRunEverything($driveLetter) {
    if (-not $hasEverything) { return }
    $label = "Everything $mode"
    Write-Host "▶ $label (drive ${driveLetter}:)" -ForegroundColor Yellow

    $indexTimes = @()
    $queryTimes = @()
    $totalTimes = @()

    # Read ini once to find drive position
    $iniContent = Get-Content -LiteralPath $EVERYTHING_INI -Raw
    $volMatch = [regex]::Match($iniContent, 'ntfs_volume_paths=(.*)')
    if (-not $volMatch.Success) {
        Write-Host "   ⚠️  ntfs_volume_paths not found in ini — skipping" -ForegroundColor Red
        return
    }
    $volPaths = $volMatch.Groups[1].Value -split ','
    $driveIdx = -1
    for ($vi = 0; $vi -lt $volPaths.Count; $vi++) {
        if ($volPaths[$vi].Trim().Trim('"') -eq "${driveLetter}:") { $driveIdx = $vi; break }
    }
    if ($driveIdx -lt 0) {
        Write-Host "   ⚠️  Drive ${driveLetter}: not in ntfs_volume_paths — skipping" -ForegroundColor Red
        return
    }
    $includesList = @(0) * $volPaths.Count
    $includesList[$driveIdx] = 1
    $includesStr = $includesList -join ","

    # Backup ini once
    $iniBak = "${EVERYTHING_INI}.bench_bak"
    if (-not (Test-Path -LiteralPath $iniBak)) {
        Copy-Item -LiteralPath $EVERYTHING_INI -Destination $iniBak -Force
    }

    # Edit ini once: only target drive, all index fields
    $c = Get-Content -LiteralPath $EVERYTHING_INI -Raw
    $c = $c -replace 'ntfs_volume_includes=.*', "ntfs_volume_includes=$includesStr"
    $c = $c -replace 'auto_include_fixed_volumes=.*', 'auto_include_fixed_volumes=0'
    $c = $c -replace 'auto_include_removable_volumes=.*', 'auto_include_removable_volumes=0'
    $c = $c -replace 'index_date_created=.*', 'index_date_created=1'
    $c = $c -replace 'index_date_accessed=.*', 'index_date_accessed=1'
    $c = $c -replace 'index_date_modified=.*', 'index_date_modified=1'
    $c = $c -replace 'index_attributes=.*', 'index_attributes=1'
    $c = $c -replace 'index_size=.*', 'index_size=1'
    $c | Out-File -FilePath $EVERYTHING_INI -Encoding ascii -NoNewline

    $everythingRunning = $false

    1..$N | ForEach-Object {
        if (-not $Cache -or -not $everythingRunning) {
            # Cold: stop → start → MFT index each round
            # Cached: only do this on first round, keep running for subsequent rounds
            Get-Process -Name "Everything" -ErrorAction SilentlyContinue |
                ForEach-Object { Stop-Process -Id $_.Id -Force -ErrorAction SilentlyContinue }
            Start-Sleep -Milliseconds 1500

            $sw = [System.Diagnostics.Stopwatch]::StartNew()
            Start-Process -FilePath $EVERYTHING_EXE -ArgumentList "-startup -minimized" -WindowStyle Hidden

            # Poll until index is ready (result count > 0)
            $indexed = $false
            $entryCount = 0
            for ($wi = 1; $wi -le 60; $wi++) {
                Start-Sleep -Milliseconds 500
                $rc = & $ES_EXE -get-result-count 2>&1
                if ($LASTEXITCODE -eq 0) {
                    [int]::TryParse($rc, [ref]$entryCount) | Out-Null
                    if ($entryCount -gt 0) { $indexed = $true; break }
                }
            }
            $sw.Stop()
            $ms = $sw.Elapsed.TotalMilliseconds

            if ($indexed) {
                $indexTimes += $ms
                $everythingRunning = $true
                if ($Cache) {
                    Write-Host ("   Run {0}: {1:N0} ms ({2:N0} entries) [cold — building cache]" -f $_, $ms, $entryCount) -ForegroundColor Gray
                } else {
                    Write-Host ("   Run {0}: {1:N0} ms ({2:N0} entries)" -f $_, $ms, $entryCount) -ForegroundColor Gray
                }
            } else {
                Write-Host "   Run $_`: timed out (30s)" -ForegroundColor Red
            }
        } else {
            # Cached: Everything already running, just measure query response time
            $sw = [System.Diagnostics.Stopwatch]::StartNew()
            $rc = & $ES_EXE -get-result-count 2>&1
            $sw.Stop()
            $ms = $sw.Elapsed.TotalMilliseconds
            $entryCount = 0; [int]::TryParse($rc, [ref]$entryCount) | Out-Null
            $indexTimes += $ms
            Write-Host ("   Run {0}: {1:N0} ms ({2:N0} entries) [cached — query only]" -f $_, $ms, $entryCount) -ForegroundColor Gray
        }
    }

    # Summary
    if ($indexTimes.Count -gt 0) {
        $avg = ($indexTimes | Measure-Object -Average).Average
        $min = ($indexTimes | Measure-Object -Minimum).Minimum
        $max = ($indexTimes | Measure-Object -Maximum).Maximum
        Write-Host ("{0,-20} avg={1,8:N0} ms   min={2,8:N0}   max={3,8:N0}" -f $label, $avg, $min, $max) -ForegroundColor Green
    }
    Write-Host ""

    # Stop Everything after benchmarking this drive (cold mode)
    # Cached mode: keep running (will be stopped when switching drives via ini edit)
    if (-not $Cache) {
        Get-Process -Name "Everything" -ErrorAction SilentlyContinue |
            ForEach-Object { Stop-Process -Id $_.Id -Force -ErrorAction SilentlyContinue }
    }
}


# ============================================
# Run benchmarks based on -Drive parameter
# ============================================

# Split extra args strings into arrays (handles "--files-only --min-size 1024")
$RustExtraArgs = if ($RustArgs) { $RustArgs -split '\s+' | Where-Object { $_ } } else { @() }
$CppExtraArgs  = if ($CppArgs)  { $CppArgs  -split '\s+' | Where-Object { $_ } } else { @() }

function RunDriveBench($driveLetter) {
    Write-Host "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━" -ForegroundColor DarkGray
    Write-Host "📁 DRIVE ${driveLetter}:" -ForegroundColor Yellow
    Write-Host "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━" -ForegroundColor DarkGray
    if (-not $CppOnly -and -not $EverythingOnly) {
        BenchRun "Rust $mode" $UFFS (@("`"$Pattern`"", '--drive', $driveLetter) + $RustExtraArgs)
    }
    if (-not $RustOnly -and -not $EverythingOnly -and (Test-Path $UFFS_CPP)) {
        BenchRun "C++ $mode" $UFFS_CPP (@("`"$Pattern`"", "--drives=$driveLetter") + $CppExtraArgs)
    }
    if (-not $RustOnly -and -not $CppOnly -and -not $NoEverything -and -not $Cache) {
        # Everything only in cold mode — cached mode would only measure es.exe IPC
        # response time (a single integer), not a full result dump like Rust/C++.
        BenchRunEverything $driveLetter
    } elseif ($Cache -and -not $NoEverything -and -not $RustOnly -and -not $CppOnly) {
        Write-Host "▶ Everything: skipped in cached mode (unfair — IPC returns count only, not full output)" -ForegroundColor DarkGray
        Write-Host ""
    }
}

function RunAllDrivesBench() {
    Write-Host "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━" -ForegroundColor DarkGray
    Write-Host "🌐 ALL DRIVES:" -ForegroundColor Yellow
    Write-Host "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━" -ForegroundColor DarkGray
    if (-not $CppOnly) {
        BenchRun "Rust $mode" $UFFS (@("`"$Pattern`"") + $RustExtraArgs)
    }
    if (-not $RustOnly -and (Test-Path $UFFS_CPP)) {
        BenchRun "C++ $mode" $UFFS_CPP (@("`"$Pattern`"") + $CppExtraArgs)
    }
}

# ============================================
# Main execution: run each drive, then "all" at the end
# ============================================

if ($AllDrives.Count -eq 0) {
    # No drives specified: just run all-drives parallel
    RunAllDrivesBench
} else {
    # Run each drive individually
    foreach ($d in $AllDrives) {
        if ($d -eq "ALL") {
            # "ALL" as a drive means run the parallel benchmark
            RunAllDrivesBench
        } else {
            RunDriveBench $d
        }
    }

    # Restore Everything ini before the all-drives run (in case per-drive benchmarks modified it)
    $benchBak = "${EVERYTHING_INI}.bench_bak"
    if (Test-Path -LiteralPath $benchBak -ErrorAction SilentlyContinue) {
        Get-Process -Name "Everything" -ErrorAction SilentlyContinue |
            ForEach-Object { Stop-Process -Id $_.Id -Force -ErrorAction SilentlyContinue }
        Start-Sleep -Seconds 1
        Copy-Item -LiteralPath $benchBak -Destination $EVERYTHING_INI -Force
        Remove-Item -LiteralPath $benchBak -Force -ErrorAction SilentlyContinue
        Write-Host "`n✅ Everything ini restored" -ForegroundColor DarkGreen
    }

    # After all individual drives, run the parallel "all drives" benchmark
    # Everything is skipped here — 25M+ files across all drives would OOM es.exe
    if (-not $NoAll) {
        Write-Host "`n" -NoNewline
        Write-Host "╔══════════════════════════════════════╗" -ForegroundColor Magenta
        Write-Host "║  FINAL: All Drives Parallel Run      ║" -ForegroundColor Magenta
        Write-Host "╚══════════════════════════════════════╝" -ForegroundColor Magenta
        RunAllDrivesBench
    }
}

# ============================================
# Restore Everything ini if we modified it
# ============================================
$benchBak = "${EVERYTHING_INI}.bench_bak"
if (Test-Path -LiteralPath $benchBak -ErrorAction SilentlyContinue) {
    Get-Process -Name "Everything" -ErrorAction SilentlyContinue |
        ForEach-Object { Stop-Process -Id $_.Id -Force -ErrorAction SilentlyContinue }
    Start-Sleep -Seconds 1
    Copy-Item -LiteralPath $benchBak -Destination $EVERYTHING_INI -Force
    Remove-Item -LiteralPath $benchBak -Force -ErrorAction SilentlyContinue
    Write-Host "`n✅ Everything ini restored from backup" -ForegroundColor DarkGreen
}

# ============================================
# SUMMARY
# ============================================
Write-Host "========================================" -ForegroundColor Cyan
Write-Host "  Benchmark Complete ($mode)" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan
if ($Cache) {
    Write-Host "`nThis measures cached performance (MFT index loaded from disk cache)." -ForegroundColor Gray
    Write-Host "Cache location: $CACHE_DIR" -ForegroundColor Gray
} else {
    Write-Host "`nThis measures fresh MFT reads (no cache)." -ForegroundColor Gray
    Write-Host "Rust saves to cache after each run, but cache is cleared before next run." -ForegroundColor Gray
    Write-Host "Note: OS filesystem cache (RAM) is NOT cleared. Later runs benefit from" -ForegroundColor DarkGray
    Write-Host "MFT data kept in RAM by Windows. C++ has no disk cache (only OS cache)." -ForegroundColor DarkGray
    Write-Host "Everything: each cold run includes startup + MFT indexing + query." -ForegroundColor DarkGray
}
