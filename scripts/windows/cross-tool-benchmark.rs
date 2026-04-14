#!/usr/bin/env rust-script
// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.
//! Cross-Tool Benchmark — UFFS (Rust) vs UFFS (C++) vs Everything
//!
//! Public-facing benchmark comparing UFFS against third-party NTFS search
//! tools on identical drives, patterns, and measurement methodology.
//!
//! # Design
//!
//!   - Each tool tested via its documented CLI — no GUI automation.
//!   - PASS/DNF: 30 s timeout → DNF.  Missing executable → SKIP.
//!   - UFFS (Rust): three phases (COLD / WARM / HOT).
//!     UFFS (C++): reads MFT every invocation (no daemon).
//!     Everything: always-hot (daemon model, index pre-loaded).
//!   - Same patterns, same drives, same result cap.
//!   - Percentile reporting: p50/p95 from N rounds per pattern.
//!
//! # Tool CLI references
//!
//!   UFFS (Rust): uffs.exe "<pattern>" --drive <X> --out=bench_out.csv --profile
//!     - Search is the default action (no "search" subcommand).
//!     - No limit — all results written to file.
//!     - Daemon model: COLD/WARM/HOT phases.
//!     - Ref: internal — see `uffs.exe --help`
//!   UFFS (C++):  uffs.com <pattern> --drives=<X>
//!     - No daemon, no --limit. Reads MFT every invocation. Outputs ALL results.
//!     - Extension filter: --ext=dll (separate flag, not glob *.dll)
//!     - Substring: *config* (glob wildcards needed)
//!     - Ref: https://github.com/githubrobbi/Ultra-Fast-File-Search
//!   Everything:  es.exe "<X>:\" <pattern> -export-csv bench_out.csv
//!     - No limit — all results written to file.
//!     - Requires Everything service running.
//!     - Ref: https://www.voidtools.com/support/everything/command_line_interface/
//!
//! # Excluded tools
//!
//!   UltraSearch (JAM Software): Evaluated but excluded.  The /CLIPBOARD /NOGUI
//!     /CLOSE flags launch a GUI process that exits before the MFT scan completes.
//!     No stdout mode, no headless search — results only go to clipboard IF the
//!     GUI renders them first.  Not viable for automated CLI benchmarking.
//!     Ref: https://manuals.jam-software.com/ultrasearch/EN/CommandLine.html
//!   WizFile: No CLI interface at all — GUI only.  Cannot be benchmarked.
//!   Windows Search: Content indexer, not MFT-based filename search.
//!
//! # Usage
//!
//! ```powershell
//! rust-script scripts\windows\cross-tool-benchmark.rs
//! rust-script scripts\windows\cross-tool-benchmark.rs --drives C,D
//! rust-script scripts\windows\cross-tool-benchmark.rs --rounds 20
//! rust-script scripts\windows\cross-tool-benchmark.rs --tools uffs,everything
//! rust-script scripts\windows\cross-tool-benchmark.rs --skip-cold
//! rust-script scripts\windows\cross-tool-benchmark.rs --uffs-bin C:\tools\uffs.exe
//! ```
//!
//! ```cargo
//! [dependencies]
//! ```
use std::env;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const TIMEOUT: Duration = Duration::from_secs(120);
const DEFAULT_ROUNDS: usize = 10;
const DEFAULT_DRIVES: &[&str] = &["C", "D"];

/// Absolute path for bench output file — avoids cwd ambiguity.
fn bench_out_path() -> String {
    let tmp = env::temp_dir();
    tmp.join("uffs_bench_out.csv").to_string_lossy().into_owned()
}
/// (label, uffs_rust_pattern, es_search, cpp_pattern, cpp_ext, validate)
/// cpp_ext: if non-empty, C++ UFFS uses `* --ext=<val>` instead of glob
/// validate: case-insensitive substring that every result line must contain
///           (empty = skip validation, e.g. full_scan)
const PATTERNS: &[(&str, &str, &str, &str, &str, &str)] = &[
    ("full_scan",  "*",           "*",           "*",           "",    ""),
    ("exact",      "notepad.exe", "notepad.exe", "notepad.exe", "",    "notepad.exe"),
    ("prefix",     "win*",        "win*",        "win*",        "",    "win"),
    ("ext_rare",   "*.dbt",       "ext:dbt",     "*.dbt",       "dbt", ".dbt"),
    ("ext_dll",    "*.dll",       "ext:dll",     "*.dll",       "dll", ".dll"),
    ("substring",  "config",      "config",      "config",      "",    "config"),
];

// ── Types ────────────────────────────────────────────────────────────────────
#[derive(Clone, Copy, PartialEq, Eq)] enum Tool { Uffs, UffsCpp, Everything }
impl Tool { fn label(self) -> &'static str { match self { Self::Uffs=>"UFFS", Self::UffsCpp=>"UFFS-C++", Self::Everything=>"Everything" } } }

#[derive(Clone, Copy, PartialEq, Eq)] enum Phase { Cold, Warm, Hot }
impl Phase { fn label(self) -> &'static str { match self { Self::Cold=>"COLD", Self::Warm=>"WARM", Self::Hot=>"HOT" } } }

#[derive(Clone, Default)]
#[allow(dead_code)] // fields read in summary output and live progress lines
struct Timing { wall_ms: u64, daemon_ms: u64, rows: u64, bad_rows: u64, ok: bool, dnf: bool, err: String }

struct Row { tool: Tool, phase: Phase, drive: String, pat: String, runs: Vec<Timing> }
struct Cfg { uffs: PathBuf, uffs_cpp: Option<PathBuf>, es: Option<PathBuf>,
             drives: Vec<String>, rounds: usize,
             tools: Vec<Tool>, skip_cold: bool }

// ── Helpers ──────────────────────────────────────────────────────────────────
fn flush() { std::io::stderr().flush().ok(); std::io::stdout().flush().ok(); }
fn fms(ms: u64) -> String {
    if ms >= 60_000 { format!("{}m{:02}s", ms/60_000, (ms%60_000)/1000) }
    else if ms >= 1000 { format!("{}.{:01}s", ms/1000, (ms%1000)/100) }
    else { format!("{ms} ms") }
}
fn p50(s: &[u64]) -> u64 { if s.is_empty() { 0 } else { s[s.len()/2] } }
fn p95(s: &[u64]) -> u64 { if s.is_empty() { 0 } else { s[(s.len() as f64 * 0.95) as usize % s.len()] } }
fn sw(runs: &[Timing]) -> Vec<u64> { let mut v: Vec<u64> = runs.iter().filter(|r| r.ok).map(|r| r.wall_ms).collect(); v.sort(); v }

// ── Discovery ────────────────────────────────────────────────────────────────
fn find_in(cs: &[PathBuf]) -> Option<PathBuf> { cs.iter().find(|p| p.exists()).cloned() }
fn where_exe(name: &str) -> Option<PathBuf> {
    Command::new("where").arg(name).output().ok().and_then(|o| {
        let s = String::from_utf8_lossy(&o.stdout);
        let l = s.lines().next().unwrap_or("").trim();
        if !l.is_empty() && Path::new(l).exists() { Some(PathBuf::from(l)) } else { None }
    })
}
fn find_uffs() -> Option<PathBuf> {
    where_exe("uffs.exe").or_else(|| {
        let h = env::var("USERPROFILE").unwrap_or_default();
        find_in(&[PathBuf::from(&h).join("bin").join("uffs.exe")])
    })
}
fn find_es() -> Option<PathBuf> {
    where_exe("es.exe").or_else(|| {
        let (h, pf, pf86) = (env::var("USERPROFILE").unwrap_or_default(),
            env::var("ProgramFiles").unwrap_or_default(),
            env::var("ProgramFiles(x86)").unwrap_or_default());
        find_in(&[
            PathBuf::from(&h).join("bin").join("es.exe"),
            PathBuf::from(&pf).join("Everything").join("es.exe"),
            PathBuf::from(&pf86).join("Everything").join("es.exe"),
            PathBuf::from(&pf).join("Everything 1.5a").join("es.exe"),
        ])
    })
}
fn find_uffs_cpp() -> Option<PathBuf> {
    where_exe("uffs.com").or_else(|| {
        let h = env::var("USERPROFILE").unwrap_or_default();
        find_in(&[PathBuf::from(&h).join("bin").join("uffs.com")])
    })
}


// ── UFFS lifecycle ───────────────────────────────────────────────────────────
fn uffs_stop(bin: &Path) {
    let _ = Command::new(bin).args(["daemon","stop"]).stdout(Stdio::null()).stderr(Stdio::null()).status();
    std::thread::sleep(Duration::from_secs(1));
}
fn uffs_purge_cache() {
    let d = PathBuf::from(env::var("LOCALAPPDATA").unwrap_or_default()).join("uffs").join("cache");
    if let Ok(es) = std::fs::read_dir(&d) {
        for e in es.flatten() {
            let p = e.path();
            if p.extension().map_or(false, |x| x == "iocp" || x == "bin") { let _ = std::fs::remove_file(&p); }
        }
    }
}


/// Check if a line is a header, footer, or empty (not a data row).
/// Matches the same logic as verify_parity.rs `is_footer_or_header_line`.
/// Handles UFFS (Rust) CSV header, UFFS (C++) header + footer, and blanks.
fn is_header_or_footer(line: &str) -> bool {
    let t = line.trim();
    t.is_empty()
        || t.starts_with("\"Path\"")              // Rust CSV header
        || t.starts_with("Path\t")                // TSV header
        || t.starts_with("Drives?")               // C++ footer
        || t.starts_with("MMMmmm that was FAST")  // C++ footer
        || t.starts_with("Search path")            // C++ footer
}

/// Count data lines in bench output file, filtering headers/footers.
/// Validates that each data line contains `needle` (case-insensitive).
/// Returns (data_rows, bad_rows).
fn count_and_validate(path: &str, needle: &str) -> (u64, u64) {
    // Try UTF-8 first, then fall back to reading raw bytes (handles UTF-16 BOM)
    let content = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(_) => {
            // Fallback: read raw bytes, strip UTF-16 LE BOM, decode lossy
            match std::fs::read(path) {
                Ok(bytes) if bytes.len() >= 2 && bytes[0] == 0xFF && bytes[1] == 0xFE => {
                    // UTF-16 LE: decode pairs of bytes
                    let u16s: Vec<u16> = bytes[2..].chunks_exact(2)
                        .map(|c| u16::from_le_bytes([c[0], c[1]]))
                        .collect();
                    String::from_utf16_lossy(&u16s)
                }
                Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
                Err(_) => return (0, 0),
            }
        }
    };
    let data: Vec<&str> = content.lines()
        .filter(|l| !is_header_or_footer(l))
        .collect();
    let total = data.len() as u64;
    if needle.is_empty() || total == 0 {
        return (total, 0);
    }
    let needle_lower = needle.to_lowercase();
    let bad = data.iter().filter(|l| !l.to_lowercase().contains(&needle_lower)).count() as u64;
    (total, bad)
}
fn cleanup_bench_file() { let p = bench_out_path(); let _ = std::fs::remove_file(&p); }

// ── Run: UFFS (Rust) ─────────────────────────────────────────────────────────
/// uffs.exe pattern --drive X --out=<tmp>/uffs_bench_out.csv --profile
/// No limit — all results written to file.  Search is the default action.
fn run_uffs(bin: &Path, drive: &str, pattern: &str, validate: &str) -> Timing {
    cleanup_bench_file();
    let bpath = bench_out_path();
    let out_arg = format!("--out={}", bpath);
    let args = [pattern, "--drive", drive, &out_arg, "--profile"];
    eprintln!("      CMD: & '{}' {}", bin.display(), args.join(" "));
    let t = Instant::now();
    let r = Command::new(bin)
        .args(args)
        .stdout(Stdio::piped()).stderr(Stdio::piped())
        .output();
    let wall = t.elapsed().as_millis() as u64;
    match r {
        Ok(o) if o.status.success() => {
            let err = String::from_utf8_lossy(&o.stderr);
            let (rows, bad_rows) = count_and_validate(&bpath, validate);
            let dms = parse_daemon_ms(&err);
            cleanup_bench_file();
            Timing { wall_ms: wall, daemon_ms: dms, rows, bad_rows, ok: true, ..Default::default() }
        }
        Ok(o) => {
            cleanup_bench_file();
            Timing { wall_ms: wall, err: String::from_utf8_lossy(&o.stderr).into(), ..Default::default() }
        }
        Err(e) => Timing { wall_ms: wall, err: e.to_string(), ..Default::default() },
    }
}
fn parse_daemon_ms(s: &str) -> u64 {
    for line in s.lines() {
        if (line.contains("Search") || line.contains("search")) && line.contains("ms") {
            let parts: Vec<&str> = line.split_whitespace().collect();
            for (i, p) in parts.iter().enumerate() {
                if *p == "ms" && i > 0 { if let Ok(v) = parts[i-1].trim_end_matches(',').parse() { return v; } }
            }
        }
    }
    0
}

// ── Run: Everything (es.exe) ─────────────────────────────────────────────────
/// es.exe "<D>:\ <pattern>" -export-csv <tmp>/uffs_bench_out.csv
/// No -n limit — all results written to file.
fn run_es(bin: &Path, drive: &str, pattern: &str, validate: &str) -> Timing {
    cleanup_bench_file();
    let bpath = bench_out_path();
    let query = if pattern == "*" { format!("{}:\\", drive) } else { format!("{}:\\ {}", drive, pattern) };
    let args = [query.as_str(), "-export-csv", bpath.as_str()];
    eprintln!("      CMD: & '{}' {}", bin.display(), args.join(" "));
    let t = Instant::now();
    let r = Command::new(bin)
        .args(args)
        .stdout(Stdio::null()).stderr(Stdio::piped())
        .output();
    let wall = t.elapsed().as_millis() as u64;
    match r {
        Ok(o) if o.status.success() => {
            let (rows, bad_rows) = count_and_validate(&bpath, validate);
            cleanup_bench_file();
            Timing { wall_ms: wall, rows, bad_rows, ok: true, ..Default::default() }
        }
        Ok(o) => {
            cleanup_bench_file();
            Timing { wall_ms: wall, err: String::from_utf8_lossy(&o.stderr).into(), ..Default::default() }
        }
        Err(e) => Timing { wall_ms: wall, err: e.to_string(), ..Default::default() },
    }
}

// ── Run: UFFS C++ (uffs.com) ─────────────────────────────────────────────────
/// C++ UFFS reads MFT every invocation (no daemon). No --limit flag.
/// Extension filter uses --ext=<ext> instead of glob *.ext.
/// Substring match needs *needle* glob wildcards.
fn run_uffs_cpp(bin: &Path, drive: &str, pattern: &str, cpp_ext: &str, validate: &str) -> Timing {
    cleanup_bench_file();
    let bpath = bench_out_path();
    let mut args: Vec<String> = Vec::new();
    if !cpp_ext.is_empty() {
        args.push("*".into());
        args.push(format!("--ext={}", cpp_ext));
    } else if !pattern.contains('*') && !pattern.contains('?') && pattern != "*" {
        args.push(format!("*{}*", pattern));
    } else {
        args.push(pattern.into());
    }
    args.push(format!("--drives={}", drive));
    args.push(format!("--out={}", bpath));
    eprintln!("      CMD: & '{}' {}", bin.display(), args.join(" "));
    let t = Instant::now();
    let r = Command::new(bin)
        .args(&args)
        .stdout(Stdio::null()).stderr(Stdio::piped())
        .output();
    let wall = t.elapsed().as_millis() as u64;
    match r {
        Ok(o) if o.status.success() => {
            let (rows, bad_rows) = count_and_validate(&bpath, validate);
            cleanup_bench_file();
            Timing { wall_ms: wall, rows, bad_rows, ok: true, ..Default::default() }
        }
        Ok(o) => {
            cleanup_bench_file();
            Timing { wall_ms: wall, err: String::from_utf8_lossy(&o.stderr).into(), ..Default::default() }
        }
        Err(e) => Timing { wall_ms: wall, err: e.to_string(), ..Default::default() },
    }
}

fn check_dnf(mut t: Timing) -> Timing {
    if t.wall_ms > TIMEOUT.as_millis() as u64 { t.dnf = true; }
    t
}

// ── Arg parsing ──────────────────────────────────────────────────────────────
fn parse_args() -> Cfg {
    let args: Vec<String> = env::args().collect();
    let mut drives: Option<Vec<String>> = None;
    let mut rounds = DEFAULT_ROUNDS;
    let mut tools_str: Option<String> = None;
    let mut skip_cold = false;
    let mut uffs_bin: Option<PathBuf> = None;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--drives" => { i += 1; drives = Some(args[i].split(',').map(|s| s.trim().to_uppercase()).collect()); }
            "--rounds" => { i += 1; rounds = args[i].parse().unwrap_or(DEFAULT_ROUNDS); }
            "--tools"  => { i += 1; tools_str = Some(args[i].clone()); }
            "--skip-cold" => { skip_cold = true; }
            "--uffs-bin" => { i += 1; uffs_bin = Some(PathBuf::from(&args[i])); }
            "--help" | "-h" => { print_help(); std::process::exit(0); }
            _ => {}
        }
        i += 1;
    }
    let uffs = uffs_bin.or_else(find_uffs).expect("ERROR: uffs.exe not found.  Use --uffs-bin <path>.");
    let uffs_cpp = find_uffs_cpp();
    let es = find_es();
    let drives = drives.unwrap_or_else(|| DEFAULT_DRIVES.iter().map(|s| s.to_string()).collect());
    let mut tools = Vec::new();
    if let Some(ts) = tools_str {
        for t in ts.split(',') {
            match t.trim().to_lowercase().as_str() {
                "uffs" => tools.push(Tool::Uffs),
                "uffs-cpp" | "uffs_cpp" | "cpp" => if uffs_cpp.is_some() { tools.push(Tool::UffsCpp); },
                "everything" | "es" => if es.is_some() { tools.push(Tool::Everything); },
                _ => eprintln!("Unknown tool: {t}"),
            }
        }
    } else {
        tools.push(Tool::Uffs);
        if uffs_cpp.is_some() { tools.push(Tool::UffsCpp); }
        if es.is_some() { tools.push(Tool::Everything); }
    }
    Cfg { uffs, uffs_cpp, es, drives, rounds, tools, skip_cold }
}

fn print_help() {
    eprintln!("Cross-Tool Benchmark — UFFS (Rust) vs UFFS (C++) vs Everything");
    eprintln!();
    eprintln!("OPTIONS:");
    eprintln!("  --drives C,D        Drives to benchmark (default: C,D)");
    eprintln!("  --rounds 10         Rounds per pattern (default: 10)");
    eprintln!("  --tools uffs,cpp,es Comma-separated tools (default: all found)");
    eprintln!("                      Values: uffs, cpp/uffs-cpp, es/everything");
    eprintln!("  --skip-cold         Skip UFFS COLD and WARM phases");
    eprintln!("  --uffs-bin <path>   Path to uffs.exe (Rust)");
    eprintln!("  --help              This message");
}

// ── Main ─────────────────────────────────────────────────────────────────────
fn main() {
    let cfg = parse_args();

    println!("╔══════════════════════════════════════════════════════════════════════════════╗");
    println!("║                     Cross-Tool Benchmark v1.0                               ║");
    println!("╠══════════════════════════════════════════════════════════════════════════════╣");
    println!("║  UFFS (Rust):  {}",  cfg.uffs.display());
    if let Some(ref cpp) = cfg.uffs_cpp { println!("║  UFFS (C++):   {}", cpp.display()); }
    else                                 { println!("║  UFFS (C++):   NOT FOUND (SKIP)"); }
    if let Some(ref es) = cfg.es   { println!("║  Everything:   {}", es.display()); }
    else                            { println!("║  Everything:   NOT FOUND (SKIP)"); }
    println!("║  Drives:       {:?}", cfg.drives);
    println!("║  Patterns:     {} queries", PATTERNS.len());
    println!("║  Rounds:       {} per pattern per tool", cfg.rounds);
    println!("║  Output:       file (--out / -export-csv → {})", bench_out_path());
    println!("║  Limit:        none (all results, fair for C++)");
    println!("║  Timeout:      {} s → DNF", TIMEOUT.as_secs());
    println!("║  Skip COLD:    {}", cfg.skip_cold);
    println!("╚══════════════════════════════════════════════════════════════════════════════╝");
    println!();

    let mut all_rows: Vec<Row> = Vec::new();

    for drive in &cfg.drives {
        println!("━━━ Drive {}:  ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━", drive);

        // ── UFFS COLD ────────────────────────────────────────────────────
        if cfg.tools.contains(&Tool::Uffs) && !cfg.skip_cold {
            eprint!("  UFFS COLD: stopping daemon, purging cache...");
            flush();
            uffs_stop(&cfg.uffs);
            uffs_purge_cache();
            eprintln!(" done.");

            for &(label, pat, _, _, _, validate) in PATTERNS {
                eprint!("    {label:<12} ");  flush();
                // COLD: only 1 round (destructive — restarts daemon each time)
                uffs_stop(&cfg.uffs);
                uffs_purge_cache();
                let t = check_dnf(run_uffs(&cfg.uffs, drive, pat, validate));
                let verdict = if t.dnf { "DNF" } else if t.bad_rows > 0 { "WRONG" } else if t.ok { "PASS" } else { "ERROR" };
                let bad_str = if t.bad_rows > 0 { format!("  bad={}", t.bad_rows) } else { String::new() };
                eprintln!("{:>8}  rows={:<8}{} {}", fms(t.wall_ms), t.rows, bad_str, verdict);
                all_rows.push(Row { tool: Tool::Uffs, phase: Phase::Cold, drive: drive.clone(),
                    pat: label.into(), runs: vec![t] });
            }
        }

        // ── UFFS WARM ────────────────────────────────────────────────────
        if cfg.tools.contains(&Tool::Uffs) && !cfg.skip_cold {
            eprint!("  UFFS WARM: stopping daemon (cache stays)...");  flush();
            uffs_stop(&cfg.uffs);
            eprintln!(" done.");

            for &(label, pat, _, _, _, validate) in PATTERNS {
                eprint!("    {label:<12} ");  flush();
                uffs_stop(&cfg.uffs);
                let t = check_dnf(run_uffs(&cfg.uffs, drive, pat, validate));
                let verdict = if t.dnf { "DNF" } else if t.bad_rows > 0 { "WRONG" } else if t.ok { "PASS" } else { "ERROR" };
                let bad_str = if t.bad_rows > 0 { format!("  bad={}", t.bad_rows) } else { String::new() };
                eprintln!("{:>8}  rows={:<8}{} {}", fms(t.wall_ms), t.rows, bad_str, verdict);
                all_rows.push(Row { tool: Tool::Uffs, phase: Phase::Warm, drive: drive.clone(),
                    pat: label.into(), runs: vec![t] });
            }
        }

        // ── UFFS HOT ────────────────────────────────────────────────────
        if cfg.tools.contains(&Tool::Uffs) {
            // Warm up daemon with a throwaway query
            let _ = run_uffs(&cfg.uffs, drive, "*", "");
            eprintln!("  UFFS HOT:  {} rounds", cfg.rounds);

            for &(label, pat, _, _, _, validate) in PATTERNS {
                eprint!("    {label:<12} ");  flush();
                let mut runs = Vec::new();
                for _ in 0..cfg.rounds {
                    runs.push(check_dnf(run_uffs(&cfg.uffs, drive, pat, validate)));
                }
                let s = sw(&runs);
                let mut dm: Vec<u64> = runs.iter().filter(|r| r.ok && r.daemon_ms > 0).map(|r| r.daemon_ms).collect();
                dm.sort();
                let any_bad = runs.iter().any(|r| r.bad_rows > 0);
                let verdict = if runs.iter().any(|r| r.dnf) { "DNF" } else if any_bad { "WRONG" } else { "PASS" };
                let daemon_str = if dm.is_empty() { String::new() } else { format!("  daemon_p50={}", fms(p50(&dm))) };
                let first_ok = runs.iter().find(|r| r.ok);
                let bad_str = first_ok.filter(|r| r.bad_rows > 0).map_or(String::new(), |r| format!("  bad={}", r.bad_rows));
                eprintln!("p50={:>6}  p95={:>6}{}  rows={}{}  {}", fms(p50(&s)), fms(p95(&s)), daemon_str, first_ok.map_or(0, |r| r.rows), bad_str, verdict);
                all_rows.push(Row { tool: Tool::Uffs, phase: Phase::Hot, drive: drive.clone(),
                    pat: label.into(), runs });
            }
        }

        // ── UFFS C++ (uffs.com) ──────────────────────────────────────────
        if cfg.tools.contains(&Tool::UffsCpp) {
            if let Some(ref cpp) = cfg.uffs_cpp {
                eprintln!("  UFFS C++ (MFT re-read, no --limit):  {} rounds", cfg.rounds);
                for &(label, pat, _, _, cpp_ext, validate) in PATTERNS {
                    eprint!("    {label:<12} ");  flush();
                    let mut runs = Vec::new();
                    for _ in 0..cfg.rounds {
                        runs.push(check_dnf(run_uffs_cpp(cpp, drive, pat, cpp_ext, validate)));
                    }
                    let s = sw(&runs);
                    let first_ok = runs.iter().find(|r| r.ok);
                    let any_bad = runs.iter().any(|r| r.bad_rows > 0);
                    let verdict = if runs.iter().any(|r| r.dnf) { "DNF" } else if any_bad { "WRONG" } else if runs.iter().all(|r| r.ok) { "PASS" } else { "ERROR" };
                    let bad_str = first_ok.filter(|r| r.bad_rows > 0).map_or(String::new(), |r| format!("  bad={}", r.bad_rows));
                    eprintln!("p50={:>6}  p95={:>6}  rows={}{}  {}", fms(p50(&s)), fms(p95(&s)), first_ok.map_or(0, |r| r.rows), bad_str, verdict);
                    all_rows.push(Row { tool: Tool::UffsCpp, phase: Phase::Hot, drive: drive.clone(),
                        pat: label.into(), runs });
                }
            }
        }

        // ── Everything ──────────────────────────────────────────────────
        if cfg.tools.contains(&Tool::Everything) {
            if let Some(ref es) = cfg.es {
                eprintln!("  Everything HOT:  {} rounds (always-hot, daemon model)", cfg.rounds);
                for &(label, _, es_pat, _, _, validate) in PATTERNS {
                    // Skip full_scan for Everything — es.exe has a 2GB IPC
                    // memory limit that crashes on drives with >2M entries.
                    // See verify_parity.rs and Everything 1.4 known limitation.
                    if label == "full_scan" {
                        eprintln!("    {label:<12} SKIP (es.exe 2GB IPC limit)");
                        continue;
                    }
                    eprint!("    {label:<12} ");  flush();
                    let mut runs = Vec::new();
                    for _ in 0..cfg.rounds {
                        runs.push(check_dnf(run_es(es, drive, es_pat, validate)));
                    }
                    let s = sw(&runs);
                    let first_ok = runs.iter().find(|r| r.ok);
                    let any_bad = runs.iter().any(|r| r.bad_rows > 0);
                    let verdict = if runs.iter().any(|r| r.dnf) { "DNF" } else if any_bad { "WRONG" } else if runs.iter().all(|r| r.ok) { "PASS" } else { "ERROR" };
                    let bad_str = first_ok.filter(|r| r.bad_rows > 0).map_or(String::new(), |r| format!("  bad={}", r.bad_rows));
                    eprintln!("p50={:>6}  p95={:>6}  rows={}{}  {}", fms(p50(&s)), fms(p95(&s)), first_ok.map_or(0, |r| r.rows), bad_str, verdict);
                    all_rows.push(Row { tool: Tool::Everything, phase: Phase::Hot, drive: drive.clone(),
                        pat: label.into(), runs });
                }
            }
        }

        println!();
    }

    // ── Summary table ────────────────────────────────────────────────────────
    print_summary(&cfg, &all_rows);
}


// ── Summary ──────────────────────────────────────────────────────────────────
fn print_summary(cfg: &Cfg, rows: &[Row]) {
    println!("╔══════════════════════════════════════════════════════════════════════════════╗");
    println!("║                           SUMMARY TABLE                                    ║");
    println!("╠══════════════════════════════════════════════════════════════════════════════╣");
    println!();

    // Header
    println!("| Drive | Tool         | Phase | Pattern      | p50      | p95      | Rows   | Bad  | Verdict |");
    println!("|-------|--------------|-------|--------------|----------|----------|--------|------|---------|");

    for row in rows {
        let s = sw(&row.runs);
        let any_dnf = row.runs.iter().any(|r| r.dnf);
        let all_ok = row.runs.iter().all(|r| r.ok);
        let any_bad = row.runs.iter().any(|r| r.bad_rows > 0);
        let verdict = if any_dnf { "DNF" } else if any_bad { "WRONG" } else if all_ok { "PASS" } else { "ERROR" };

        let p50_str = if s.is_empty() { "—".to_string() } else { fms(p50(&s)) };
        let p95_str = if s.is_empty() { "—".to_string() } else { fms(p95(&s)) };
        let rows_str = row.runs.iter().find(|r| r.ok).map_or("—".into(), |r| format!("{}", r.rows));
        let bad_str = row.runs.iter().find(|r| r.ok).map_or("—".into(), |r| {
            if r.bad_rows == 0 { "0".into() } else { format!("{}", r.bad_rows) }
        });

        // Print any errors from failed runs
        for r in &row.runs {
            if !r.ok && !r.err.is_empty() {
                eprintln!("  ⚠ {} {} {}/{}: {}", row.tool.label(), row.phase.label(), row.drive, row.pat, r.err);
            }
            if r.bad_rows > 0 {
                eprintln!("  ⚠ {} {} {}/{}: {} rows failed validation", row.tool.label(), row.phase.label(), row.drive, row.pat, r.bad_rows);
            }
        }

        println!("| {:<5} | {:<12} | {:<5} | {:<12} | {:>8} | {:>8} | {:>6} | {:>4} | {:<7} |",
            format!("{}:", row.drive), row.tool.label(), row.phase.label(),
            row.pat, p50_str, p95_str, rows_str, bad_str, verdict);
    }

    println!();

    // ── Cross-tool comparison (HOT only) ─────────────────────────────────
    println!("╔══════════════════════════════════════════════════════════════════════════════╗");
    println!("║                     HOT COMPARISON (head-to-head)                          ║");
    println!("╚══════════════════════════════════════════════════════════════════════════════╝");
    println!();
    println!("| Drive | Pattern      | UFFS HOT p50 | UFFS-C++ p50 | Everything p50 |");
    println!("|-------|--------------|-------------|--------------|----------------|");

    for drive in &cfg.drives {
        for &(label, _, _, _, _, _) in PATTERNS {
            let uffs_p50 = find_p50(rows, Tool::Uffs, Phase::Hot, drive, label);
            let cpp_p50 = find_p50(rows, Tool::UffsCpp, Phase::Hot, drive, label);
            let es_p50 = find_p50(rows, Tool::Everything, Phase::Hot, drive, label);
            println!("| {:<5} | {:<12} | {:>11} | {:>12} | {:>14} |",
                format!("{}:", drive), label, uffs_p50, cpp_p50, es_p50);
        }
    }

    println!();
    println!("Legend:  PASS = completed within {}s.  DNF = timed out.  SKIP = tool not found.", TIMEOUT.as_secs());
    println!("Note:   All tools write ALL results to file (no limit).  Fair I/O for all.");
    println!("        UFFS (Rust) has three phases: COLD (no cache), WARM (cache), HOT (daemon).");
    println!("        UFFS (C++) re-reads MFT every invocation (no daemon).");
    println!("        Everything is always-hot (daemon model).");
    println!("        UltraSearch excluded — no functional headless CLI (see script header).");
}

fn find_p50(rows: &[Row], tool: Tool, phase: Phase, drive: &str, pat: &str) -> String {
    rows.iter()
        .find(|r| r.tool == tool && r.phase == phase && r.drive == drive && r.pat == pat)
        .map(|r| {
            let s = sw(&r.runs);
            if s.is_empty() { "—".to_string() } else { fms(p50(&s)) }
        })
        .unwrap_or_else(|| "SKIP".to_string())
}