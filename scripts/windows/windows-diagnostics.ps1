# SPDX-License-Identifier: MPL-2.0
# Copyright (c) 2025-2026 SKY, LLC.
#
# Windows Diagnostics — ETW/WPR traces, Defender analyzer, DLL import audit.
#
# Phase 2 measurement toolkit for UFFS CLI performance.
# These measurements require Windows-native APIs and tooling.
#
# Usage:
#   # Run all diagnostics (requires Administrator for WPR/Defender)
#   .\scripts\windows\windows-diagnostics.ps1
#
#   # Run specific sections
#   .\scripts\windows\windows-diagnostics.ps1 -Mode imports
#   .\scripts\windows\windows-diagnostics.ps1 -Mode defender
#   .\scripts\windows\windows-diagnostics.ps1 -Mode etw
#   .\scripts\windows\windows-diagnostics.ps1 -Mode binary-info
#
#   # Specify custom binary path
#   .\scripts\windows\windows-diagnostics.ps1 -UffsBin C:\tools\uffs.exe
#
# Prerequisites:
#   - Windows 10+ / Windows 11
#   - WPR/WPA (Windows Performance Toolkit — part of Windows SDK or ADK)
#   - Administrator privileges for ETW and Defender recording
#   - Visual Studio Build Tools (for dumpbin) — optional

param(
    [ValidateSet("all", "imports", "defender", "etw", "binary-info")]
    [string]$Mode = "all",

    [string]$UffsBin = "",
    [string]$OutputDir = "$env:TEMP\uffs-diagnostics",
    [int]$LaunchCount = 30,
    [string]$Drive = "C"
)

$ErrorActionPreference = "Continue"
Set-StrictMode -Version Latest

# Detect whether we're running elevated (needed for ETW/Defender sections).
$isAdmin = ([Security.Principal.WindowsPrincipal] `
    [Security.Principal.WindowsIdentity]::GetCurrent()
).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)

# ── Helpers ──────────────────────────────────────────────────────────────────

function Write-Header($title) {
    $line = "=" * 72
    Write-Host "`n$line" -ForegroundColor Cyan
    Write-Host "  $title" -ForegroundColor Cyan
    Write-Host "$line`n" -ForegroundColor Cyan
}

function Write-SubHeader($title) {
    Write-Host "`n── $title ──`n" -ForegroundColor Yellow
}

function Find-Binary($name) {
    $found = Get-Command $name -ErrorAction SilentlyContinue
    if ($found) { return $found.Source }
    # Check common paths
    $paths = @(
        "$env:USERPROFILE\bin\$name",
        "$env:ProgramFiles\$name",
        "$env:ProgramFiles\Everything\$name"
    )
    foreach ($p in $paths) {
        if (Test-Path $p) { return $p }
    }
    return $null
}

function Format-Size($bytes) {
    if ($bytes -ge 1MB) { "{0:N1} MB" -f ($bytes / 1MB) }
    elseif ($bytes -ge 1KB) { "{0:N0} KB" -f ($bytes / 1KB) }
    else { "$bytes B" }
}

# ── Setup ────────────────────────────────────────────────────────────────────

if (-not (Test-Path $OutputDir)) { New-Item -ItemType Directory -Path $OutputDir -Force | Out-Null }

# Find uffs.exe
if ($UffsBin -eq "") {
    $UffsBin = Find-Binary "uffs.exe"
    if (-not $UffsBin) {
        Write-Error "uffs.exe not found. Specify with -UffsBin parameter."
        exit 1
    }
}

if (-not (Test-Path $UffsBin)) {
    Write-Error "Binary not found: $UffsBin"
    exit 1
}

$uffsSize = (Get-Item $UffsBin).Length
Write-Host "╔══════════════════════════════════════════════════════════════╗"
Write-Host "║  UFFS Windows Diagnostics — Phase 2 Measurement Toolkit    ║"
Write-Host "╚══════════════════════════════════════════════════════════════╝"
Write-Host ""
Write-Host "  Mode:       $Mode"
Write-Host "  uffs.exe:   $UffsBin ($(Format-Size $uffsSize))"
Write-Host "  Output:     $OutputDir"
Write-Host "  Launches:   $LaunchCount"
Write-Host "  Drive:      $Drive"
Write-Host ""


# ═══════════════════════════════════════════════════════════════════════════════
# SECTION 1: Binary Info & PE Analysis
# ═══════════════════════════════════════════════════════════════════════════════

function Run-BinaryInfo {
    Write-Header "BINARY INFO & PE ANALYSIS"

    # Basic file info
    Write-SubHeader "File Properties"
    $file = Get-Item $UffsBin
    Write-Host "  Path:       $($file.FullName)"
    Write-Host "  Size:       $(Format-Size $file.Length) ($($file.Length) bytes)"
    Write-Host "  Created:    $($file.CreationTime)"
    Write-Host "  Modified:   $($file.LastWriteTime)"
    Write-Host "  Version:    $(try { $file.VersionInfo.FileVersion } catch { 'N/A' })"

    # PE header analysis via PowerShell
    Write-SubHeader "PE Header (basic)"
    try {
        $bytes = [System.IO.File]::ReadAllBytes($UffsBin)
        $peOffset = [BitConverter]::ToInt32($bytes, 0x3C)
        $machine = [BitConverter]::ToUInt16($bytes, $peOffset + 4)
        $sections = [BitConverter]::ToUInt16($bytes, $peOffset + 6)
        $optHdrSize = [BitConverter]::ToUInt16($bytes, $peOffset + 20)
        $subsystem = [BitConverter]::ToUInt16($bytes, $peOffset + 24 + 68)

        $machineStr = switch ($machine) {
            0x8664 { "x86-64 (AMD64)" }
            0x14c  { "x86 (i386)" }
            0xAA64 { "ARM64" }
            default { "Unknown (0x{0:X4})" -f $machine }
        }
        $subsysStr = switch ($subsystem) {
            3 { "WINDOWS_CUI (console)" }
            2 { "WINDOWS_GUI" }
            default { "Unknown ($subsystem)" }
        }

        Write-Host "  Machine:    $machineStr"
        Write-Host "  Sections:   $sections"
        Write-Host "  Subsystem:  $subsysStr"
    } catch {
        Write-Host "  (PE parse failed: $_)"
    }

    # Authenticode signature check
    Write-SubHeader "Code Signing"
    try {
        $sig = Get-AuthenticodeSignature $UffsBin
        Write-Host "  Status:     $($sig.Status)"
        Write-Host "  Signer:     $($sig.SignerCertificate.Subject)"
    } catch {
        Write-Host "  (Not signed or check failed)"
    }
}

# ═══════════════════════════════════════════════════════════════════════════════
# SECTION 2: DLL Import Audit
# ═══════════════════════════════════════════════════════════════════════════════

function Run-ImportAudit {
    Write-Header "DLL IMPORT AUDIT"

    # Try dumpbin first (Visual Studio)
    $dumpbin = $null
    $vsWhere = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe"
    if (Test-Path $vsWhere) {
        $vsPath = & $vsWhere -latest -property installationPath 2>$null
        if ($vsPath) {
            $candidates = Get-ChildItem "$vsPath\VC\Tools\MSVC\*\bin\Hostx64\x64\dumpbin.exe" -ErrorAction SilentlyContinue |
                Sort-Object -Descending | Select-Object -First 1
            if ($candidates) { $dumpbin = $candidates.FullName }
        }
    }

    if ($dumpbin -and (Test-Path $dumpbin)) {
        Write-SubHeader "Import Table (via dumpbin)"

        $importOutput = & $dumpbin /IMPORTS $UffsBin 2>&1 | Out-String
        $outputFile = Join-Path $OutputDir "imports-full.txt"
        $importOutput | Out-File $outputFile -Encoding UTF8
        Write-Host "  Full output saved to: $outputFile"

        # Parse DLL names and function counts
        $dllPattern = '^\s+(\S+\.dll)\s*$'
        $funcPattern = '^\s+[0-9A-F]+\s+[0-9A-F]+\s+\S+'
        $currentDll = ""
        $dllFuncs = @{}

        foreach ($line in $importOutput -split "`n") {
            if ($line -match $dllPattern) {
                $currentDll = $Matches[1].ToLower()
                if (-not $dllFuncs.ContainsKey($currentDll)) {
                    $dllFuncs[$currentDll] = 0
                }
            } elseif ($currentDll -and $line -match $funcPattern) {
                $dllFuncs[$currentDll]++
            }
        }

        Write-Host ""
        Write-Host ("  {0,-35} {1,10} {2,15}" -f "DLL", "Functions", "Hot path?")
        Write-Host "  $('-' * 65)"
        $knownHot = @("kernel32.dll", "ntdll.dll", "ucrtbase.dll", "vcruntime140.dll", "msvcrt.dll")
        $knownCold = @("ws2_32.dll", "advapi32.dll", "bcrypt.dll", "secur32.dll")

        foreach ($dll in $dllFuncs.GetEnumerator() | Sort-Object -Property Value -Descending) {
            $hotness = if ($knownHot -contains $dll.Key) { "YES" }
                       elseif ($knownCold -contains $dll.Key) { "delay-load?" }
                       else { "check" }
            Write-Host ("  {0,-35} {1,10} {2,15}" -f $dll.Key, $dll.Value, $hotness)
        }

        Write-Host "`n  Total DLLs imported: $($dllFuncs.Count)"

        # Check for Winsock
        if ($dllFuncs.ContainsKey("ws2_32.dll")) {
            Write-Host "`n  ⚠ ws2_32.dll (Winsock) is imported — this pulls in the socket stack." -ForegroundColor Yellow
            Write-Host "    If AF_UNIX is the only user, switching to named pipes would remove this." -ForegroundColor Yellow
        }

        # Delay-load section
        Write-SubHeader "Delay-Loaded DLLs (via dumpbin)"
        $delayOutput = & $dumpbin /DIRECTIVES $UffsBin 2>&1 | Out-String
        if ($delayOutput -match "DELAYLOAD") {
            Write-Host "  Delay-loaded DLLs found:"
            foreach ($line in $delayOutput -split "`n") {
                if ($line -match "DELAYLOAD:(\S+)") {
                    Write-Host "    $($Matches[1])"
                }
            }
        } else {
            Write-Host "  No delay-loaded DLLs found."
        }
    } else {
        Write-SubHeader "Import Table (via PowerShell PE parser)"
        Write-Host "  dumpbin not found (install Visual Studio Build Tools for detailed analysis)."
        Write-Host "  Using basic PE import directory parsing..."

        # Basic PE import parsing
        try {
            $bytes = [System.IO.File]::ReadAllBytes($UffsBin)
            $peOffset = [BitConverter]::ToInt32($bytes, 0x3C)
            # Optional header offset
            $optOffset = $peOffset + 24
            $magic = [BitConverter]::ToUInt16($bytes, $optOffset)
            $is64 = ($magic -eq 0x20B)

            if ($is64) {
                $importRva = [BitConverter]::ToUInt32($bytes, $optOffset + 120)
            } else {
                $importRva = [BitConverter]::ToUInt32($bytes, $optOffset + 104)
            }

            Write-Host "  Import Directory RVA: 0x$($importRva.ToString('X8'))"
            Write-Host "  (For full import list, install Visual Studio Build Tools and re-run)"
        } catch {
            Write-Host "  (PE parse failed: $_)"
        }
    }
}

# ═══════════════════════════════════════════════════════════════════════════════
# SECTION 3: ETW/WPR Startup Trace
# ═══════════════════════════════════════════════════════════════════════════════

function Run-EtwTrace {
    Write-Header "ETW/WPR STARTUP TRACE"

    if (-not $isAdmin) {
        Write-Warning "Skipping ETW trace — requires Administrator privileges."
        Write-Host "  Re-run as Administrator to capture ETW traces."
        return
    }

    # Check for WPR
    $wpr = Get-Command "wpr.exe" -ErrorAction SilentlyContinue
    if (-not $wpr) {
        Write-Error "WPR (Windows Performance Recorder) not found."
        Write-Host "  Install Windows Performance Toolkit (part of Windows SDK or ADK)."
        Write-Host "  Download: https://learn.microsoft.com/en-us/windows-hardware/get-started/adk-install"
        return
    }

    $etlFile = Join-Path $OutputDir "uffs-startup.etl"

    Write-SubHeader "Step 1: List available WPR profiles"
    & wpr -profiles 2>&1 | ForEach-Object { Write-Host "    $_" }

    Write-SubHeader "Step 2: Start WPR recording"
    Write-Host "  Starting file-mode recording with GeneralProfile + CPU..."
    Write-Host "  (This captures process creation, image loads, hard faults, CPU)"
    try {
        & wpr -start GeneralProfile -start CPU -filemode 2>&1
        Write-Host "  Recording started." -ForegroundColor Green
    } catch {
        Write-Error "Failed to start WPR: $_"
        return
    }

    Write-SubHeader "Step 3: Launch uffs.exe $LaunchCount times"
    $timings = @()
    for ($i = 1; $i -le $LaunchCount; $i++) {
        $sw = [Diagnostics.Stopwatch]::StartNew()
        $proc = Start-Process -FilePath $UffsBin -ArgumentList "version" `
            -NoNewWindow -Wait -PassThru -RedirectStandardOutput "NUL" 2>$null
        $sw.Stop()
        $timings += $sw.ElapsedMilliseconds
        if ($i % 10 -eq 0) { Write-Host "    $i / $LaunchCount completed" }
    }

    Write-SubHeader "Step 4: Stop WPR recording"
    try {
        & wpr -stop $etlFile "UFFS startup profiling - $LaunchCount launches" 2>&1
        Write-Host "  ETL saved to: $etlFile" -ForegroundColor Green
        Write-Host "  Size: $(Format-Size (Get-Item $etlFile).Length)"
    } catch {
        Write-Error "Failed to stop WPR: $_"
    }

    Write-SubHeader "Launch timing summary"
    $sorted = $timings | Sort-Object
    $p50 = $sorted[[int]($sorted.Count / 2)]
    $p95 = $sorted[[int]($sorted.Count * 0.95)]
    $min = $sorted[0]
    $max = $sorted[-1]

    Write-Host "  Launches:  $LaunchCount"
    Write-Host "  p50:       $p50 ms"
    Write-Host "  p95:       $p95 ms"
    Write-Host "  min:       $min ms"
    Write-Host "  max:       $max ms"

    Write-SubHeader "Next steps"
    Write-Host "  1. Open the ETL file in WPA (Windows Performance Analyzer):"
    Write-Host "       wpa `"$etlFile`""
    Write-Host ""
    Write-Host "  2. In WPA, inspect these views for uffs.exe:"
    Write-Host "       - Process Lifetime → process start/end times"
    Write-Host "       - Images → loaded DLLs with sizes and load times"
    Write-Host "       - CPU Usage (Precise) → where CPU time goes"
    Write-Host "       - Hard Faults → page faults during image load"
    Write-Host "       - File I/O → file operations during startup"
    Write-Host "       - Minifilter I/O → filter driver activity (AV, etc.)"
    Write-Host ""
    Write-Host "  3. Filter to process name = uffs.exe"
    Write-Host "  4. Look for the time between process start and first user-mode code"
}

# ═══════════════════════════════════════════════════════════════════════════════
# SECTION 4: Defender Performance Analysis
# ═══════════════════════════════════════════════════════════════════════════════

function Run-DefenderAnalysis {
    Write-Header "DEFENDER PERFORMANCE ANALYSIS"

    if (-not $isAdmin) {
        Write-Warning "Skipping Defender analysis — requires Administrator privileges."
        return
    }

    # Check for Defender cmdlets
    $hasCmdlet = Get-Command "New-MpPerformanceRecording" -ErrorAction SilentlyContinue
    if (-not $hasCmdlet) {
        Write-Warning "Defender performance cmdlets not available."
        Write-Host "  Requires Windows Defender with performance recording support."
        Write-Host "  Available on Windows 10 21H2+ / Windows 11."
        return
    }

    $defenderEtl = Join-Path $OutputDir "uffs-defender.etl"

    Write-SubHeader "Step 1: Start Defender performance recording"
    Write-Host "  Recording Defender scan activity..."
    try {
        New-MpPerformanceRecording -RecordTo $defenderEtl
    } catch {
        # New-MpPerformanceRecording is interactive — it records until Ctrl+C
        # We'll use a background job instead
    }

    # Alternative: time-boxed approach
    Write-Host ""
    Write-Host "  NOTE: New-MpPerformanceRecording is interactive."
    Write-Host "  Running automated approach instead..."
    Write-Host ""

    $job = Start-Job -ScriptBlock {
        param($etl)
        New-MpPerformanceRecording -RecordTo $etl
    } -ArgumentList $defenderEtl

    Write-SubHeader "Step 2: Launch uffs.exe $LaunchCount times during recording"
    Start-Sleep -Seconds 2  # Let recording stabilize

    for ($i = 1; $i -le $LaunchCount; $i++) {
        & $UffsBin version 2>$null | Out-Null
        if ($i % 10 -eq 0) { Write-Host "    $i / $LaunchCount completed" }
    }

    Start-Sleep -Seconds 2  # Let recording capture final events
    Stop-Job $job -ErrorAction SilentlyContinue
    Remove-Job $job -Force -ErrorAction SilentlyContinue

    if (Test-Path $defenderEtl) {
        Write-SubHeader "Step 3: Generate Defender performance report"
        try {
            $report = Get-MpPerformanceReport -Path $defenderEtl `
                -TopProcesses 10 -TopFiles 20 -TopScans 20

            $reportFile = Join-Path $OutputDir "defender-report.txt"
            $report | Out-File $reportFile -Encoding UTF8
            Write-Host "  Report saved to: $reportFile"
            Write-Host ""
            Write-Host "  Top processes by scan impact:"
            $report | Select-Object -First 20 | ForEach-Object { Write-Host "    $_" }
        } catch {
            Write-Warning "Failed to generate report: $_"
            Write-Host "  Try manually:"
            Write-Host "    Get-MpPerformanceReport -Path `"$defenderEtl`" -TopProcesses 10 -TopFiles 20"
        }
    } else {
        Write-Warning "Defender ETL not created. Try running manually:"
        Write-Host "  New-MpPerformanceRecording -RecordTo `"$defenderEtl`""
        Write-Host "  # In another terminal: run uffs.exe version 30 times"
        Write-Host "  # Press Ctrl+C to stop recording"
        Write-Host "  Get-MpPerformanceReport -Path `"$defenderEtl`" -TopProcesses 10 -TopFiles 20"
    }
}

# ═══════════════════════════════════════════════════════════════════════════════
# MAIN DISPATCHER
# ═══════════════════════════════════════════════════════════════════════════════

switch ($Mode) {
    "binary-info" {
        Run-BinaryInfo
    }
    "imports" {
        Run-ImportAudit
    }
    "etw" {
        Run-EtwTrace
    }
    "defender" {
        Run-DefenderAnalysis
    }
    "all" {
        Run-BinaryInfo
        Run-ImportAudit
        if ($isAdmin) {
            Run-EtwTrace
            Run-DefenderAnalysis
        }
    }
}

Write-Host "`n  Done. Copy results into docs/research/perf-phase2-measurement-plan.md" -ForegroundColor Green
