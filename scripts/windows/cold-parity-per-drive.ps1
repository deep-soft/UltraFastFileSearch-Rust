# =============================================================================
# Per-drive COLD parity benchmark: UFFS Rust v0.5.66 vs C++ reference
# Reproduces the v0.4.106 methodology documented in
#   docs/architecture/engine/11-performance-deep-dive.md
#   "Parity Comparison (v0.4.106 historical, COLD, 6 drives, sequential per-drive)"
#
# Sequence per drive (one drive at a time, sequential):
#   1. Stop daemon, delete that drive's compact + raw cache files
#   2. Run UFFS Rust COLD (raw MFT read + compact index build + cache write)
#   3. Run C++ reference on same drive (OS page cache now warm from step 2)
#   4. Record wall-clock + record count + files/sec
#
# Usage (from elevated PowerShell, project root):
#   .\scripts\windows\cold-parity-per-drive.ps1 -Drives C,D,E,F,M,S,G
#   .\scripts\windows\cold-parity-per-drive.ps1 -Drives C,D -OutputFile LOG\Output_cold_parity.txt
#   .\scripts\windows\cold-parity-per-drive.ps1 -UffsBin $HOME\bin\uffs.exe -CppBin $HOME\bin\uffs.com
#   .\scripts\windows\cold-parity-per-drive.ps1 -SkipCpp                # Rust-only run
#   .\scripts\windows\cold-parity-per-drive.ps1 -DumpRaw                # include full --profile output per drive
#
# Binary resolution (auto-fallback when -UffsBin/-CppBin are not passed):
#   1. Explicit path via -UffsBin / -CppBin
#   2. $HOME\bin\uffs.exe / $HOME\bin\uffs.com      (user's local install)
#   3. bare 'uffs.exe' / 'uffs.com' (resolved via PATH)
#
# Requires:
#   - UFFS Rust binary reachable via one of the paths above
#   - UFFS C++ reference binary (uffs.com) reachable (optional; -SkipCpp bypasses)
#   - Admin elevation (MFT read)
# =============================================================================

[CmdletBinding()]
param(
    [string[]] $Drives       = @('C','D','E','F','M','S','G'),
    [string]   $UffsBin      = '',
    [string]   $CppBin       = '',
    [string]   $OutputFile   = 'LOG\Output_cold_parity.txt',
    [int]      $SleepBetween = 2,
    [switch]   $SkipCpp,
    [switch]   $DumpRaw
)

$ErrorActionPreference = 'Stop'
$CacheDir = Join-Path $env:LOCALAPPDATA 'uffs\cache'

# ---------- binary resolution ------------------------------------------------

function Resolve-UffsBinary {
    param(
        [string] $Explicit,
        [string] $HomeBinName,
        [string] $PathName
    )
    # 1. Explicit -UffsBin / -CppBin wins if provided.
    if ($Explicit) {
        if (Test-Path -LiteralPath $Explicit) { return (Resolve-Path -LiteralPath $Explicit).Path }
        # Still honour the explicit path even if not-yet-present — error
        # surfaces later during invocation, which is clearer than silently
        # swapping to a different binary.
        return $Explicit
    }
    # 2. $HOME\bin\<name> (user's local install).
    $homeBin = Join-Path $HOME "bin\$HomeBinName"
    if (Test-Path -LiteralPath $homeBin) { return $homeBin }
    # 3. PATH lookup.
    $cmd = Get-Command $PathName -ErrorAction SilentlyContinue
    if ($cmd) { return $cmd.Source }
    # 4. Fall back to bare name — invocation will fail with a clear error.
    return $PathName
}

$UffsBin = Resolve-UffsBinary -Explicit $UffsBin -HomeBinName 'uffs.exe' -PathName 'uffs.exe'
$CppBin  = Resolve-UffsBinary -Explicit $CppBin  -HomeBinName 'uffs.com' -PathName 'uffs.com'

function Test-Invokable {
    param([string] $Target)
    if (Test-Path -LiteralPath $Target) { return $true }
    return [bool](Get-Command $Target -ErrorAction SilentlyContinue)
}

# ---------- helpers ---------------------------------------------------------

function Write-Divider {
    param([string] $Title)
    $line = '=' * 118
    Write-Host ''
    Write-Host $line -ForegroundColor Cyan
    if ($Title) { Write-Host "  $Title" -ForegroundColor Cyan }
    Write-Host $line -ForegroundColor Cyan
    Write-Host ''
}

function Stop-UffsDaemon {
    try { & $UffsBin daemon stop 2>&1 | Out-Null } catch {}
    Start-Sleep -Milliseconds 500
    # Fallback: kill any stray uffs-daemon process
    Get-Process uffs-daemon -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
    Start-Sleep -Milliseconds 500
}

function Remove-DriveCache {
    param([string] $Drive)
    if (-not (Test-Path $CacheDir)) { return }
    $patterns = @(
        "${Drive}_compact.uffs",
        "${Drive}_index.uffs",
        "${Drive}_index.uffs.tmp",
        "${Drive}_index.lock",
        "${Drive}_compact.uffs.tmp"
    )
    foreach ($p in $patterns) {
        $path = Join-Path $CacheDir $p
        if (Test-Path $path) {
            Remove-Item $path -Force -ErrorAction SilentlyContinue
            if ($DumpRaw) { Write-Host "    - removed $path" -ForegroundColor DarkGray }
        }
    }
}

function Get-DaemonTotalRecords {
    # `uffs daemon stats` prints "Total records: N" with thousands
    # separators. Returns $null if the daemon isn't running or the line
    # isn't found.
    try {
        $statsOut = & $UffsBin daemon stats 2>&1 | Out-String
        if ($statsOut -match 'Total records:\s+([0-9,]+)') {
            return [int64]($matches[1] -replace ',', '')
        }
    } catch {}
    return $null
}

function Invoke-UffsCold {
    param([string] $Drive)

    Write-Host "  [Rust COLD] $UffsBin `"*`" --drive $Drive --profile --limit 100" -ForegroundColor Yellow
    $sw = [System.Diagnostics.Stopwatch]::StartNew()
    # --profile writes to stderr. Capture both so the transcript shows
    # the full daemon-side breakdown for post-hoc analysis.
    $output = & $UffsBin "*" --drive $Drive --profile --limit 100 2>&1 | Out-String
    $sw.Stop()
    $elapsed = $sw.Elapsed.TotalSeconds

    # Authoritative record count: ask the daemon how many records it
    # loaded for this drive. --profile only prints "Rows returned: 100"
    # (the --limit cap), not the total drive size.
    $records = Get-DaemonTotalRecords
    $filesSec = if ($records) { [int]($records / $elapsed) } else { $null }

    # Parse --profile output so we can report both wall-clock
    # (matches v0.4.106 methodology) and the daemon-internal sub-phases
    # (useful for root-causing cold-path regressions).
    #   Connect:         X ms              <- CLI-to-daemon handshake
    #   Await ready:     X ms              <- daemon spawn + MFT read + index build (THE cold number)
    #   Search (IPC):    X ms (daemon: Y ms) <- search only, trivial on cold since drive is loaded
    $connectMs = if ($output -match 'Connect:\s+(\d+)\s+ms')      { [int]$matches[1] } else { $null }
    $readyMs   = if ($output -match 'Await ready:\s+(\d+)\s+ms')  { [int]$matches[1] } else { $null }
    $ipcMs     = if ($output -match 'Search \(IPC\):\s+(\d+)\s+ms') { [int]$matches[1] } else { $null }
    $daemonMs  = if ($output -match 'daemon:\s+(\d+)\s+ms')       { [int]$matches[1] } else { $null }

    [pscustomobject]@{
        Tool      = 'UFFS-Rust-v0.5.66'
        Phase     = 'COLD'
        Drive     = $Drive
        Seconds   = [math]::Round($elapsed, 2)
        Records   = $records
        FilesSec  = $filesSec
        ConnectMs = $connectMs
        ReadyMs   = $readyMs
        IpcMs     = $ipcMs
        DaemonMs  = $daemonMs
        RawOut    = $output
    }
}

function Invoke-UffsCppWarmDisk {
    param([string] $Drive)

    if ($SkipCpp) { return $null }
    if (-not (Test-Invokable $CppBin)) {
        Write-Host "  [C++] $CppBin not found — skipping" -ForegroundColor DarkYellow
        return $null
    }

    $tmpOut = Join-Path $env:TEMP "cpp_${Drive}_$([guid]::NewGuid().ToString('N')).csv"
    Write-Host "  [C++ warm-disk] $CppBin `"*`" --drives=$Drive --columns=path > $tmpOut" -ForegroundColor Yellow

    $sw = [System.Diagnostics.Stopwatch]::StartNew()
    & $CppBin "*" "--drives=$Drive" "--columns=path" > $tmpOut 2>&1
    $sw.Stop()

    # Row count from output file
    $records = $null
    if (Test-Path $tmpOut) {
        try {
            $records = (Get-Content $tmpOut -ReadCount 1000 | Measure-Object -Line).Lines
        } catch {}
        Remove-Item $tmpOut -Force -ErrorAction SilentlyContinue
    }

    $elapsed = $sw.Elapsed.TotalSeconds
    $filesSec = if ($records) { [int]($records / $elapsed) } else { $null }

    [pscustomobject]@{
        Tool     = 'UFFS-CPP-reference'
        Phase    = 'WARM-DISK'
        Drive    = $Drive
        Seconds  = [math]::Round($elapsed, 2)
        Records  = $records
        FilesSec = $filesSec
        RawOut   = ''
    }
}

# ---------- preflight -------------------------------------------------------

# Ensure output dir exists
$outDir = Split-Path -Parent $OutputFile
if ($outDir -and -not (Test-Path $outDir)) { New-Item -ItemType Directory -Path $outDir -Force | Out-Null }

# Start a transcript so we capture EVERYTHING to the LOG file
Start-Transcript -Path $OutputFile -Force | Out-Null

Write-Divider "UFFS Cold-parity benchmark — $(Get-Date -Format 'yyyy-MM-dd HH:mm:ss K')"
Write-Host "  UffsBin      : $UffsBin"
Write-Host "  CppBin       : $(if ($SkipCpp) { '(skipped)' } else { $CppBin })"
Write-Host "  Drives       : $($Drives -join ', ')"
Write-Host "  CacheDir     : $CacheDir"
Write-Host "  OutputFile   : $OutputFile"
Write-Host "  SleepBetween : $SleepBetween s"
Write-Host ''

# Verify binaries: the resolver above returned either a full path or a
# bare name; Test-Invokable (defined near the top) handles both.
if (-not (Test-Invokable $UffsBin)) {
    Write-Host "ERROR: UFFS Rust binary '$UffsBin' not found." -ForegroundColor Red
    Write-Host "       Looked in (in order): explicit -UffsBin, $HOME\bin\uffs.exe, PATH." -ForegroundColor Red
    Write-Host "       Pass -UffsBin <full-path> or add the binary to one of those locations." -ForegroundColor Red
    Stop-Transcript | Out-Null
    exit 1
}

& $UffsBin --version 2>&1 | Out-String | Write-Host
if (-not $SkipCpp -and (Test-Invokable $CppBin)) {
    & $CppBin --version 2>&1 | Out-String | Write-Host
} elseif (-not $SkipCpp) {
    Write-Host "  (C++ reference '$CppBin' not found — will skip C++ column per drive)" -ForegroundColor DarkYellow
}

# ---------- main loop -------------------------------------------------------

$results = [System.Collections.Generic.List[pscustomobject]]::new()

foreach ($drive in $Drives) {
    Write-Divider "Drive $drive"

    Write-Host "  [step 1] stop daemon + purge $drive cache files"
    Stop-UffsDaemon
    Remove-DriveCache -Drive $drive
    Start-Sleep -Seconds $SleepBetween

    Write-Host "  [step 2] UFFS Rust COLD"
    $r = Invoke-UffsCold -Drive $drive
    $results.Add($r)
    $recStr = if ($r.Records) { '{0:N0}' -f $r.Records } else { 'n/a' }
    $fpsStr = if ($r.FilesSec) { '{0:N0}/s' -f $r.FilesSec } else { 'n/a' }
    Write-Host ('    -> wall={0}s  records={1}  files/sec={2}' -f $r.Seconds, $recStr, $fpsStr) -ForegroundColor Green
    if ($null -ne $r.ReadyMs) {
        Write-Host ('       breakdown: connect={0}ms  await_ready={1}ms  ipc={2}ms  daemon_search={3}ms' `
            -f $r.ConnectMs, $r.ReadyMs, $r.IpcMs, $r.DaemonMs) -ForegroundColor DarkGray
    }
    if ($DumpRaw) { Write-Host $r.RawOut -ForegroundColor DarkGray }

    Write-Host ''
    Write-Host "  [step 3] UFFS C++ reference (warm disk — OS page cache now populated by step 2)"
    $c = Invoke-UffsCppWarmDisk -Drive $drive
    if ($c) {
        $results.Add($c)
        Write-Host ('    -> {0}s, {1} records, {2}/s' -f $c.Seconds, $c.Records, $c.FilesSec) -ForegroundColor Green
    }

    Start-Sleep -Seconds $SleepBetween
}

# ---------- summary table ---------------------------------------------------

Write-Divider 'Summary — table 1: doc-compatible (matches v0.4.106 schema)'

$byDrive = $results | Group-Object Drive
$totalRustSec = 0.0
$totalCppSec  = 0.0

Write-Host '| Drive | C++ (warm disk) | Rust (cold) | Ratio | Files/sec (Rust) |'
Write-Host '|-------|-----------------|-------------|-------|------------------|'
foreach ($grp in $byDrive) {
    $rust = $grp.Group | Where-Object Tool -eq 'UFFS-Rust-v0.5.66' | Select-Object -First 1
    $cpp  = $grp.Group | Where-Object Tool -eq 'UFFS-CPP-reference' | Select-Object -First 1
    $rustSec = if ($rust) { $rust.Seconds } else { 0 }
    $cppSec  = if ($cpp)  { $cpp.Seconds  } else { 0 }
    $totalRustSec += $rustSec
    $totalCppSec  += $cppSec
    $ratio = if ($cppSec -gt 0 -and $rustSec -gt 0) { [math]::Round($rustSec / $cppSec, 2) } else { 'n/a' }
    $cppCell   = if ($cpp)  { "$cppSec s"  } else { '(skipped)' }
    $rustCell  = if ($rust) { "$rustSec s" } else { 'n/a' }
    $filesCell = if ($rust.FilesSec) { '{0:N0}/s' -f $rust.FilesSec } else { 'n/a' }
    Write-Host ('| {0}: | {1} | {2} | {3}x | {4} |' -f $grp.Name, $cppCell, $rustCell, $ratio, $filesCell)
}

$totalRatio = if ($totalCppSec -gt 0) { [math]::Round($totalRustSec / $totalCppSec, 2) } else { 'n/a' }
Write-Host ('| **TOTAL (sequential)** | **{0:N1} s** | **{1:N1} s** | **{2}x** | — |' -f $totalCppSec, $totalRustSec, $totalRatio)

Write-Divider 'Summary — table 2: Rust cold-path sub-phase breakdown'

Write-Host '| Drive | Records | Wall | AwaitReady | IPC | Daemon | CLI tax | Notes |'
Write-Host '|-------|--------:|-----:|-----------:|----:|-------:|--------:|-------|'
foreach ($grp in $byDrive) {
    $rust = $grp.Group | Where-Object Tool -eq 'UFFS-Rust-v0.5.66' | Select-Object -First 1
    if (-not $rust) { continue }
    $cliTax = if ($null -ne $rust.ReadyMs -and $null -ne $rust.ConnectMs -and $null -ne $rust.IpcMs) {
        [int](($rust.Seconds * 1000) - $rust.ReadyMs - $rust.IpcMs - $rust.ConnectMs)
    } else { 'n/a' }
    $recStr = if ($rust.Records) { '{0:N0}' -f $rust.Records } else { 'n/a' }
    Write-Host ('| {0}: | {1} | {2} s | {3} ms | {4} ms | {5} ms | {6} ms | |' -f `
        $grp.Name, $recStr, $rust.Seconds, $rust.ReadyMs, $rust.IpcMs, $rust.DaemonMs, $cliTax)
}
Write-Host ''
Write-Host '  Legend:'
Write-Host '    Wall        = Stopwatch-measured wall-clock (matches v0.4.106 methodology)'
Write-Host '    AwaitReady  = daemon spawn + MFT read + compact index build (the COLD cost)'
Write-Host '    IPC         = client round-trip for the * --limit 100 search'
Write-Host '    Daemon      = daemon-side search time (microseconds on cold since drive is already loaded)'
Write-Host '    CLI tax     = Wall - AwaitReady - IPC - Connect (process startup + output formatting)'

Write-Divider 'Done'
Stop-Transcript | Out-Null
Write-Host "Full log written to: $OutputFile" -ForegroundColor Cyan
