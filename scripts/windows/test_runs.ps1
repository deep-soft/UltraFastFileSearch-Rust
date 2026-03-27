# test_runs.ps1 - UFFS Live Data Collection (Windows only)
#
# Purpose:
#   Collect live MFT data and scan outputs on Windows for offline analysis on Mac.
#   This script focuses on LIVE data collection only - offline analysis is done on Mac.
#
# Strategy:
#   - Never write binary outputs with Set-Content.
#   - Capture stdout/stderr to .log files (text) with diagnostic logging.
#   - Sequential per physical disk; parallel across physical disks (PS7+).
#   - Enable diagnostic logging for live path analysis.
#   - ALWAYS save IOCP capture (.iocp) for each drive - captures real Windows IOCP order
#   - ALWAYS save uncompressed MFT snapshot (.bin) as fallback for offline analysis
#
# What gets collected:
#   1. IOCP captures (.iocp files) - captures IOCP completion order for 100% accurate replay
#   2. MFT snapshots (uncompressed .bin files) - fallback for sequential offline analysis
#   3. C++ baseline scan output - reference for parity comparison
#   4. Rust LIVE scan output + diagnostic logs - with chunk/record processing stats
#
# Diagnostic logging captures (in .log files):
#   - Chunk handoff, record boundaries, preload_concurrent timing
#   - USA fixup success/failure, records parsed, records not in-use
#   - Parallel sync (lock acquisition), chunk processing order
#
# After running this script, transfer all files to Mac for offline analysis using:
#   - uffs "*" --mft-file <mft_file.iocp> --drive <letter> --parity-compat  (IOCP replay)
#   - uffs "*" --mft-file <mft_file.bin> --drive <letter> --parity-compat   (sequential fallback)
#   - rust-script scripts/verify_parity.rs <data_dir> <drive> --regenerate
#   - See: TESTING_TOOLS_GUIDE.md for full workflow
[CmdletBinding()]
param(
    [string[]]$Drives = @(),       # Drives to test (empty = auto-detect NTFS drives)
    [switch]$SkipMftExtras,        # Skip extra MFT formats (compressed, raw) - uncompressed always saved
    [string]$BinDir = "",          # Custom bin directory (default: $HOME\bin)
    [int]$ThrottleLimit = 2,       # Max physical disks in parallel (PS7+ only)
    [switch]$VerboseLog            # Enable verbose/trace logging (more detail, larger logs)
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$env:RUST_BACKTRACE = "full"

# Logging configuration for C++ algorithm parity analysis
# Modules:
#   uffs_mft::cpp_tree        - C++ tree metrics algorithm port
#   uffs_mft::cpp_types       - C++ types and parsing structures
#   uffs_mft::cpp_io_pipeline - C++ I/O pipeline (bitmap sync, chunk processing)
#   uffs_mft::parse           - MFT record parsing
#   uffs_mft::io              - I/O operations
#   uffs_mft::reader          - MFT reader
#   uffs_mft::index           - Index building and tree metrics
#   uffs_cli::commands        - CLI command execution
#
# Levels: error < warn < info < debug < trace
#
# IMPORTANT: The post-tree diagnostic for LIVE issues uses tracing::warn!
# so it will appear even at "warn" level. The tripwire logs use tracing::debug!
# so they require at least "debug" level to appear.
if ($VerboseLog) {
    # TRACE: Maximum verbosity - all C++ algorithm modules at trace level
    $env:RUST_LOG = "uffs_mft=trace,uffs_cli=trace,uffs_core=trace"
    Write-Host "📋 Verbose logging enabled (TRACE level for all uffs modules)" -ForegroundColor Yellow
} else {
    # Default: warn level - captures post-tree diagnostics for LIVE issues
    # The "[tree] FINAL: directories with descendants==0" warning will appear here
    $env:RUST_LOG = "warn"
    Write-Host "📋 Standard logging (warn level - captures tree diagnostics, use -VerboseLog for trace)" -ForegroundColor Yellow
}

$WorkDir  = Get-Location
$FinalLog = Join-Path $WorkDir "test_runs.md"
$TempLog  = Join-Path $WorkDir "test_runs.md.tmp"

function Format-FileSize {
    param([long]$Bytes)
    if ($Bytes -ge 1GB) { return "{0:N2} GB" -f ($Bytes / 1GB) }
    if ($Bytes -ge 1MB) { return "{0:N2} MB" -f ($Bytes / 1MB) }
    if ($Bytes -ge 1KB) { return "{0:N2} KB" -f ($Bytes / 1KB) }
    return "$Bytes bytes"
}

function Get-NtfsDrives {
    Get-WmiObject Win32_LogicalDisk |
            Where-Object { $_.DriveType -eq 3 -and $_.FileSystem -eq "NTFS" } |
            ForEach-Object { $_.DeviceID.TrimEnd(':') }
}

# Best-effort mapping: Drive letter -> Physical disk number
# Requires Storage module (usually present on Win10/11). If it fails, we return $null for that drive.
function Get-PhysicalDiskNumberForDrive {
    param([string]$DriveLetter)

    try {
        $part = Get-Partition -DriveLetter $DriveLetter -ErrorAction Stop
        $disk = Get-Disk -Number $part.DiskNumber -ErrorAction Stop
        return [int]$disk.Number
    } catch {
        return $null
    }
}

# Simple markdown logger (single writer)
$fs = New-Object System.IO.FileStream(
$TempLog,
[System.IO.FileMode]::Create,
[System.IO.FileAccess]::Write,
[System.IO.FileShare]::ReadWrite
)
$sw = New-Object System.IO.StreamWriter($fs, [System.Text.Encoding]::UTF8)
$sw.NewLine = "`r`n"
function LogLine { param([string]$Line="") $sw.WriteLine($Line); $sw.Flush() }

# Run a command and write stdout/stderr to a TEXT log file.
# Does NOT try to "write output files" itself.
function Invoke-CmdToLog {
    param(
        [string]$Title,
        [string]$CommandLine,
        [string]$LogFileName
    )

    $logPath = Join-Path $WorkDir $LogFileName
    $started = Get-Date
    $exitCode = 0

    Write-Host "  → $Title..." -NoNewline

    try {
        # Capture cmd.exe output lines, then write to log (text)
        $lines = @(& cmd.exe /c $CommandLine 2>&1)
        $exitCode = $LASTEXITCODE
        $lines | Set-Content -LiteralPath $logPath -Encoding UTF8
    } catch {
        $exitCode = -1
        @("PowerShell exception:", $_.Exception.ToString()) | Set-Content -LiteralPath $logPath -Encoding UTF8
    }

    $ended = Get-Date
    $durMs = [math]::Round((New-TimeSpan -Start $started -End $ended).TotalMilliseconds)

    if ($exitCode -eq 0) { Write-Host " ✅ ($durMs ms)" -ForegroundColor Green }
    else { Write-Host " ❌ (exit: $exitCode)" -ForegroundColor Red }

    return [pscustomobject]@{
        Title      = $Title
        Command    = $CommandLine
        LogFile    = $LogFileName
        Started    = $started
        Ended      = $ended
        DurationMs = $durMs
        ExitCode   = $exitCode
    }
}

try {
    Write-Host ""
    Write-Host "╔════════════════════════════════════════╗" -ForegroundColor Cyan
    Write-Host "║    UFFS Test Run — Data Collection      ║" -ForegroundColor Cyan
    Write-Host "╚════════════════════════════════════════╝" -ForegroundColor Cyan
    Write-Host ""

    LogLine "# UFFS Test Run Report (data collection only)"
    LogLine ""
    LogLine ("- **Started:** " + (Get-Date -Format o))
    LogLine ("- **Working dir:** " + $WorkDir.ToString())
    LogLine ("- **User:** " + (whoami))
    LogLine ("- **Computer:** " + $env:COMPUTERNAME)
    LogLine ("- **PowerShell:** " + $PSVersionTable.PSVersion.ToString())
    LogLine ""

    if (-not $BinDir) { $BinDir = Join-Path $HOME "bin" }

    $UffsExe    = Join-Path $BinDir "uffs.exe"
    $UffsCom    = Join-Path $BinDir "uffs.com"
    $UffsMftExe = Join-Path $BinDir "uffs_mft.exe"

    # Everything CLI (es.exe) — gold-standard reference for MFT-based search
    # Try common locations: PATH, Everything install dir, user bin dir
    $EsExe = $null
    $esCandidates = @(
        (Get-Command "es.exe" -ErrorAction SilentlyContinue | Select-Object -ExpandProperty Source -ErrorAction SilentlyContinue),
        (Join-Path $BinDir "es.exe"),
        (Join-Path ${env:ProgramFiles} "Everything\es.exe"),
        (Join-Path ${env:ProgramW6432} "Everything\es.exe"),
        (Join-Path ${env:LOCALAPPDATA} "Everything\es.exe")
    ) | Where-Object { $_ -and (Test-Path -LiteralPath $_ -ErrorAction SilentlyContinue) }
    if ($esCandidates.Count -gt 0) { $EsExe = $esCandidates[0] }

    $hasRust = Test-Path -LiteralPath $UffsExe
    $hasCpp  = Test-Path -LiteralPath $UffsCom
    $hasMft  = Test-Path -LiteralPath $UffsMftExe
    $hasEs   = $null -ne $EsExe

    LogLine "## Binaries"
    LogLine ""
    LogLine "| Binary | Path | Exists |"
    LogLine "|--------|------|--------|"
    LogLine ("| uffs.exe (Rust) | ``$UffsExe`` | " + $(if ($hasRust) { "✅" } else { "❌" }) + " |")
    LogLine ("| uffs.com (C++) | ``$UffsCom`` | " + $(if ($hasCpp) { "✅" } else { "❌" }) + " |")
    LogLine ("| uffs_mft.exe | ``$UffsMftExe`` | " + $(if ($hasMft) { "✅" } else { "❌" }) + " |")
    LogLine ("| es.exe (Everything) | ``$(if ($EsExe) { $EsExe } else { '(not found)' })`` | " + $(if ($hasEs) { "✅" } else { "⏭️ optional" }) + " |")
    LogLine ""
    if (-not $hasEs) {
        Write-Host "  ⏭️ es.exe (Everything CLI) not found — Everything baseline will be skipped" -ForegroundColor DarkGray
        Write-Host "    Install from: https://www.voidtools.com/ (CLI downloads)" -ForegroundColor DarkGray
    }

    if ($Drives.Count -eq 0) {
        $Drives = @(Get-NtfsDrives)
        Write-Host "Auto-detected NTFS drives: $($Drives -join ', ')" -ForegroundColor Yellow
    }

    LogLine ("**Drives to test:** " + ($Drives -join ", "))
    LogLine ""

    # Version check
    $timings = @()
    if ($hasRust) {
        $timings += Invoke-CmdToLog -Title "uffs --version" `
            -CommandLine ("`"$UffsExe`" --version") `
            -LogFileName "uffs_version.log"
        LogLine "- Version log: ``uffs_version.log``"
        LogLine ""

    }

    # MFT saves - ALWAYS save IOCP capture (primary) and uncompressed MFT (fallback)
    # IOCP capture is preferred as it captures real Windows IOCP completion order
    # Extra formats (compressed, raw) can be skipped with -SkipMftExtras
    if ($hasMft -and $Drives.Count -gt 0) {
        LogLine "---"
        LogLine ""
        LogLine "# MFT Snapshots"
        LogLine ""

        foreach ($mftDrive in $Drives) {
            Write-Host "MFT Save (Drive $mftDrive)..." -ForegroundColor Cyan

            $mftIocp = "${mftDrive}_mft.iocp"       # IOCP capture - captures real IOCP order (PRIMARY)
            $mftNoCompress = "${mftDrive}_mft.bin"  # Uncompressed sequential (fallback)

            # Always save IOCP capture (captures real Windows IOCP completion order)
            # This is the PRIMARY format for 100% accurate LIVE replay on Mac
            $timings += Invoke-CmdToLog -Title "uffs_mft save (IOCP capture): drive $mftDrive" `
                -CommandLine ("`"$UffsMftExe`" save --drive $mftDrive --output `"$mftIocp`" --iocp") `
                -LogFileName "${mftDrive}_mft_save_iocp.log"

            # Always save uncompressed MFT (fallback for sequential offline analysis)
            $timings += Invoke-CmdToLog -Title "uffs_mft save (uncompressed): drive $mftDrive" `
                -CommandLine ("`"$UffsMftExe`" save --drive $mftDrive --output `"$mftNoCompress`" --no-compress") `
                -LogFileName "${mftDrive}_mft_save.log"

            # Extra formats (optional)
            if (-not $SkipMftExtras) {
                $mftCompressed = "${mftDrive}_mft_compressed.bin"
                $mftRaw        = "${mftDrive}_mft.raw"

                $timings += Invoke-CmdToLog -Title "uffs_mft save (compressed): drive $mftDrive" `
                    -CommandLine ("`"$UffsMftExe`" save --drive $mftDrive -o `"$mftCompressed`"") `
                    -LogFileName "${mftDrive}_mft_save_compressed.log"

                $timings += Invoke-CmdToLog -Title "uffs_mft save (raw): drive $mftDrive" `
                    -CommandLine ("`"$UffsMftExe`" save --drive $mftDrive -o `"$mftRaw`" --raw") `
                    -LogFileName "${mftDrive}_mft_save_raw.log"
            }
        }

        LogLine "### Generated MFT Files"
        LogLine ""
        LogLine "| Drive | File | Format | Size |"
        LogLine "|-------|------|--------|------|"
        foreach ($mftDrive in $Drives) {
            # IOCP capture (primary)
            $mftIocp = "${mftDrive}_mft.iocp"
            $p = Join-Path $WorkDir $mftIocp
            if (Test-Path -LiteralPath $p) {
                $size = (Get-Item -LiteralPath $p).Length
                LogLine "| $mftDrive | $mftIocp | **IOCP capture** (primary) | $(Format-FileSize $size) |"
            } else {
                LogLine "| $mftDrive | $mftIocp | IOCP capture | (missing) |"
            }

            # Uncompressed MFT (fallback)
            $mftNoCompress = "${mftDrive}_mft.bin"
            $p = Join-Path $WorkDir $mftNoCompress
            if (Test-Path -LiteralPath $p) {
                $size = (Get-Item -LiteralPath $p).Length
                LogLine "| $mftDrive | $mftNoCompress | Sequential (fallback) | $(Format-FileSize $size) |"
            } else {
                LogLine "| $mftDrive | $mftNoCompress | Sequential | (missing) |"
            }

            if (-not $SkipMftExtras) {
                $mftCompressed = "${mftDrive}_mft_compressed.bin"
                $mftRaw        = "${mftDrive}_mft.raw"
                foreach ($f in @($mftCompressed, $mftRaw)) {
                    $p = Join-Path $WorkDir $f
                    if (Test-Path -LiteralPath $p) {
                        $size = (Get-Item -LiteralPath $p).Length
                        LogLine "| $mftDrive | $f | Extra | $(Format-FileSize $size) |"
                    } else {
                        LogLine "| $mftDrive | $f | Extra | (missing) |"
                    }
                }
            }
        }
        LogLine ""
    }

    # Group drives by physical disk number (best effort)
    $driveToDisk = @{}
    foreach ($d in $Drives) {
        $driveToDisk[$d] = Get-PhysicalDiskNumberForDrive -DriveLetter $d
    }

    $allMapped = $true
    foreach ($d in $Drives) {
        if ($null -eq $driveToDisk[$d]) { $allMapped = $false; break }
    }

    $isPS7Plus = ($PSVersionTable.PSVersion.Major -ge 7)

    LogLine "---"
    LogLine ""
    LogLine "# Drive Scans"
    LogLine ""
    LogLine ("- **PS7+ available:** " + $(if ($isPS7Plus) { "Yes" } else { "No" }))
    LogLine ("- **Physical disk mapping available:** " + $(if ($allMapped) { "Yes" } else { "No (falling back to sequential)" }))
    LogLine ("- **Policy:** sequential per physical disk; parallel across disks")
    LogLine ""

    # Build disk groups: @{ diskNumber = @('D','E') }
    $diskGroups = @{}
    if ($allMapped) {
        foreach ($d in $Drives) {
            $diskNum = $driveToDisk[$d]
            if (-not $diskGroups.ContainsKey($diskNum)) { $diskGroups[$diskNum] = @() }
            $diskGroups[$diskNum] += $d
        }
    }

    # Worker logic (sequential for drives within a disk group)
    # Focus on: C++ baseline, Rust live (cpp io), Rust offline (from saved MFT)
    $runDiskGroup = {
        param(
            [int]$DiskNumber,
            [string[]]$GroupDrives,
            [string]$WorkDir,
            [string]$UffsExe,
            [string]$UffsCom,
            [string]$UffsMftExe,
            [string]$EsExe,
            [bool]$HasRust,
            [bool]$HasCpp,
            [bool]$HasMft,
            [bool]$HasEs
        )

        $groupResults = @()

        foreach ($Drive in $GroupDrives) {
            $driveLower = $Drive.ToLower()

            # Output files
            $cppOut         = "cpp_${driveLower}.txt"
            $rustLiveOut    = "rust_live_${driveLower}.txt"
            $rustLiveTraceOut = "rust_live_trace_${driveLower}.txt"
            $rustOfflineOut = "rust_offline_${driveLower}.txt"
            $esOut          = "es_${driveLower}.txt"

            # Log files (capture stderr with diagnostics)
            $cppLog         = "cpp_${driveLower}.log"
            $rustLiveLog    = "rust_live_${driveLower}.log"
            $rustLiveTraceLog = "rust_live_trace_${driveLower}.log"
            $rustOfflineLog = "rust_offline_${driveLower}.log"
            $esLog          = "es_${driveLower}.log"

            # MFT file for offline comparison
            $mftBin = "${driveLower}_mft.bin"

            function Run-LoggedLocal {
                param([string]$Title, [string]$CmdLine, [string]$LogFileName, [string]$OutFileName = "")

                $logPath = Join-Path $WorkDir $LogFileName
                $started = Get-Date
                $exitCode = 0

                Write-Host "  → $Title..." -NoNewline

                try {
                    # Run command with stdout to output file, stderr to log file
                    # This properly separates scan output from diagnostic logs
                    if ($OutFileName) {
                        $outPath = Join-Path $WorkDir $OutFileName
                        # Use cmd.exe to properly separate stdout (>output) and stderr (2>log)
                        & cmd.exe /c "$CmdLine > `"$outPath`" 2> `"$logPath`""
                        $exitCode = $LASTEXITCODE
                    } else {
                        # No output file - capture everything to log
                        $lines = @(& cmd.exe /c $CmdLine 2>&1)
                        $exitCode = $LASTEXITCODE
                        $lines | Set-Content -LiteralPath $logPath -Encoding UTF8
                    }
                } catch {
                    $exitCode = -1
                    @("PowerShell exception:", $_.Exception.ToString()) | Set-Content -LiteralPath $logPath -Encoding UTF8
                }

                $ended = Get-Date
                $durMs = [math]::Round((New-TimeSpan -Start $started -End $ended).TotalMilliseconds)

                if ($exitCode -eq 0) {
                    Write-Host " ✅ ($durMs ms)" -ForegroundColor Green
                } else {
                    Write-Host " ❌ (exit: $exitCode, $durMs ms)" -ForegroundColor Red
                    # Show log content on error
                    Write-Host "    📋 Log ($LogFileName):" -ForegroundColor Yellow
                    if (Test-Path -LiteralPath $logPath) {
                        $logContent = Get-Content -LiteralPath $logPath -TotalCount 20
                        foreach ($line in $logContent) {
                            Write-Host "       $line" -ForegroundColor DarkYellow
                        }
                        $totalLines = (Get-Content -LiteralPath $logPath | Measure-Object -Line).Lines
                        if ($totalLines -gt 20) {
                            Write-Host "       ... ($($totalLines - 20) more lines in $LogFileName)" -ForegroundColor DarkYellow
                        }
                    } else {
                        Write-Host "       (log file not found)" -ForegroundColor DarkYellow
                    }
                }

                return [pscustomobject]@{
                    Drive      = $Drive
                    Title      = $Title
                    Command    = $CmdLine
                    LogFile    = $LogFileName
                    OutFile    = $OutFileName
                    DurationMs = $durMs
                    ExitCode   = $exitCode
                }
            }

            $runs = @()

            # 0. Clear Rust cache for this drive (ensures fresh MFT read with current algorithms)
            if ($HasMft) {
                Write-Host "  → Clearing Rust cache for drive $Drive..." -NoNewline
                & cmd.exe /c "`"$UffsMftExe`" cache-clear --drive $Drive" 2>&1 | Out-Null
                Write-Host " ✅" -ForegroundColor Green
            }

            # 1. C++ baseline (no diagnostics, just output)
            # C++ always reads MFT fresh (no caching)
            if ($HasCpp) {
                $runs += Run-LoggedLocal -Title "C++ (baseline): drive $Drive" `
                    -CmdLine ("`"$UffsCom`" `"*`" --drives=$Drive") `
                    -LogFileName $cppLog `
                    -OutFileName $cppOut
            } else {
                $runs += [pscustomobject]@{ Drive=$Drive; Title="C++ (baseline)"; Command=""; LogFile=$cppLog; OutFile=$cppOut; DurationMs=$null; ExitCode=$null }
            }

            # 2. Rust LIVE scan (with diagnostic logging via RUST_LOG)
            # --no-cache forces fresh MFT read to ensure tree metrics are computed
            # --parity-compat --format custom: match C++ output format for parity comparison
            if ($HasRust) {
                $runs += Run-LoggedLocal -Title "Rust LIVE: drive $Drive" `
                    -CmdLine ("`"$UffsExe`" `"*`" --drive $Drive --no-cache --parity-compat --format custom") `
                    -LogFileName $rustLiveLog `
                    -OutFileName $rustLiveOut
            } else {
                $runs += [pscustomobject]@{ Drive=$Drive; Title="Rust LIVE"; Command=""; LogFile=$rustLiveLog; OutFile=$rustLiveOut; DurationMs=$null; ExitCode=$null }
            }

            # 2b. Rust LIVE scan with DEBUG logging (for detailed diagnostics)
            # Temporarily enables debug-level logging to capture detailed diagnostics
            # NOTE: Trace logging can cause stack overflow on deep directory trees - use debug level instead
            if ($HasRust) {
                $savedRustLog = $env:RUST_LOG
                $env:RUST_LOG = "uffs_mft=debug,uffs_cli=debug,uffs_core=debug"
                $runs += Run-LoggedLocal -Title "Rust LIVE TRACE: drive $Drive" `
                    -CmdLine ("`"$UffsExe`" `"*`" --drive $Drive --no-cache --parity-compat --format custom") `
                    -LogFileName $rustLiveTraceLog `
                    -OutFileName $rustLiveTraceOut
                $env:RUST_LOG = $savedRustLog
            } else {
                $runs += [pscustomobject]@{ Drive=$Drive; Title="Rust LIVE TRACE"; Command=""; LogFile=$rustLiveTraceLog; OutFile=$rustLiveTraceOut; DurationMs=$null; ExitCode=$null }
            }

            # 3. Everything (es.exe) — gold-standard reference baseline
            # Uses Everything's own MFT index to list all files on the drive
            # Output: full path, one per line, sorted by name (default es.exe behavior)
            # Everything must be running for es.exe to work (IPC)
            if ($HasEs) {
                # -path "X:\" restricts to this drive, -s sorts by full path
                # -no-header suppresses CSV header if csv mode were used
                $runs += Run-LoggedLocal -Title "Everything (es.exe): drive $Drive" `
                    -CmdLine ("`"$EsExe`" -path `"${Drive}:\`" -s -name -path-column -size -date-created -date-modified -date-accessed -no-digit-grouping -csv -no-header") `
                    -LogFileName $esLog `
                    -OutFileName $esOut
            } else {
                Write-Host "  → Everything (es.exe): skipped (not installed)" -ForegroundColor DarkGray
                $runs += [pscustomobject]@{ Drive=$Drive; Title="Everything (es.exe)"; Command=""; LogFile=$esLog; OutFile=$esOut; DurationMs=$null; ExitCode=$null }
            }

            # 4. Rust OFFLINE scan - SKIPPED on Windows
            # Offline analysis is done on Mac for faster iteration (see TESTING_TOOLS_GUIDE.md)
            Write-Host "  → Rust OFFLINE: skipped (offline analysis done on Mac)" -ForegroundColor DarkGray

            $groupResults += [pscustomobject]@{
                Disk   = $DiskNumber
                Drive  = $Drive
                Files  = [pscustomobject]@{ Cpp=$cppOut; RustLive=$rustLiveOut; RustLiveTrace=$rustLiveTraceOut; RustOffline=$rustOfflineOut; Es=$esOut }
                Logs   = [pscustomobject]@{ Cpp=$cppLog; RustLive=$rustLiveLog; RustLiveTrace=$rustLiveTraceLog; RustOffline=$rustOfflineLog; Es=$esLog }
                Runs   = $runs
            }
        }

        return $groupResults
    }

    $scanResults = @()

    if (-not $allMapped -or -not $isPS7Plus -or $Drives.Count -le 1) {
        # Safe fallback: fully sequential
        Write-Host "Drive scans: running sequential (single drive / PS<7 / mapping unavailable)." -ForegroundColor Yellow

        foreach ($d in $Drives) {
            # treat each drive as its own "disk group"
            $scanResults += & $runDiskGroup -DiskNumber -1 -GroupDrives @($d) -WorkDir $WorkDir `
                -UffsExe $UffsExe -UffsCom $UffsCom -UffsMftExe $UffsMftExe -EsExe "$EsExe" `
                -HasRust $hasRust -HasCpp $hasCpp -HasMft $hasMft -HasEs $hasEs
        }
    } else {
        # Parallel across physical disks; sequential within each disk
        Write-Host "Drive scans: parallel across physical disks (ThrottleLimit=$ThrottleLimit), sequential within each disk." -ForegroundColor Yellow

        $diskNumbers = @($diskGroups.Keys | Sort-Object)
        $scanResults = $diskNumbers | ForEach-Object -Parallel {
            $diskNum = $_
            $allDiskGroups = $using:diskGroups
            $groupDrives = $allDiskGroups[$diskNum]
            & $using:runDiskGroup -DiskNumber $diskNum -GroupDrives $groupDrives -WorkDir $using:WorkDir `
                -UffsExe $using:UffsExe -UffsCom $using:UffsCom -UffsMftExe $using:UffsMftExe -EsExe "$($using:EsExe)" `
                -HasRust $using:hasRust -HasCpp $using:hasCpp -HasMft $using:hasMft -HasEs $using:hasEs
        } -ThrottleLimit $ThrottleLimit
    }

    # Consolidate results into markdown (single thread)
    LogLine "---"
    LogLine ""
    LogLine "# Scan Outputs"
    LogLine ""

    foreach ($r in $scanResults) {
        $drive = $r.Drive
        $disk  = $r.Disk

        LogLine "## Drive $drive (Disk $disk)"
        LogLine ""

        LogLine "| Flow | Output file | Size | Log file | Exit | Duration (ms) |"
        LogLine "|------|-------------|------|----------|------|---------------:|"

        foreach ($run in $r.Runs) {
            $outFile = $run.OutFile
            $outPath = if ($outFile) { Join-Path $WorkDir $outFile } else { $null }
            $sizeStr = "N/A"
            if ($outPath -and (Test-Path -LiteralPath $outPath)) {
                $sizeStr = Format-FileSize (Get-Item -LiteralPath $outPath).Length
            }

            $logPath = if ($run.LogFile) { Join-Path $WorkDir $run.LogFile } else { $null }
            $logSizeStr = ""
            if ($logPath -and (Test-Path -LiteralPath $logPath)) {
                $logSize = (Get-Item -LiteralPath $logPath).Length
                $logSizeStr = " ($(Format-FileSize $logSize))"
            }

            $exit = if ($null -eq $run.ExitCode) { "skipped" } else { "$($run.ExitCode)" }
            $dur  = if ($null -eq $run.DurationMs) { "N/A" } else { "$($run.DurationMs)" }

            LogLine "| $($run.Title) | $outFile | $sizeStr | $($run.LogFile)$logSizeStr | $exit | $dur |"
        }

        LogLine ""
    }

    LogLine "---"
    LogLine ("**Completed:** " + (Get-Date -Format o))
}
finally {
    if ($sw) { $sw.Flush(); $sw.Dispose() }
    if ($fs) { $fs.Dispose() }

    try {
        if (Test-Path -LiteralPath $FinalLog) { Remove-Item -LiteralPath $FinalLog -Force }
        Move-Item -LiteralPath $TempLog -Destination $FinalLog -Force
        Write-Host ""
        Write-Host "📄 Report written: $FinalLog" -ForegroundColor Cyan
    }
    catch {
        $fallback = Join-Path $WorkDir ("test_runs_" + (Get-Date -Format "yyyyMMdd_HHmmss") + ".md")
        Move-Item -LiteralPath $TempLog -Destination $fallback -Force
        Write-Warning ("test_runs.md was locked; wrote: " + $fallback)
    }
}
