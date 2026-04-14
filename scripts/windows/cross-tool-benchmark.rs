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
//!   UFFS (Rust): uffs.exe search "<pattern>" --drive <X> --limit <N> --profile
//!     - Daemon model: COLD/WARM/HOT phases.
//!     - Ref: internal — see `uffs.exe --help`
//!   UFFS (C++):  uffs.com <pattern> --drives=<X>
//!     - No daemon, no --limit. Reads MFT every invocation. Outputs ALL results.
//!     - Extension filter: --ext=dll (separate flag, not glob *.dll)
//!     - Substring: *config* (glob wildcards needed)
//!     - Ref: https://github.com/githubrobbi/Ultra-Fast-File-Search
//!   Everything:  es.exe "<X>:\" <pattern> -n <N>
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

const TIMEOUT: Duration = Duration::from_secs(30);
const DEFAULT_LIMIT: u32 = 100;
const DEFAULT_ROUNDS: usize = 10;
const DEFAULT_DRIVES: &[&str] = &["C", "D"];
/// (label, uffs_rust_pattern, es_search, us_pattern, cpp_ext_override)
/// cpp_ext_override: if non-empty, C++ UFFS uses `* --ext=<val>` instead of glob
const PATTERNS: &[(&str, &str, &str, &str, &str)] = &[
    ("full_scan",  "*",           "*",           "*",           ""),
    ("exact",      "notepad.exe", "notepad.exe", "notepad.exe", ""),
    ("prefix",     "win*",        "win*",        "win*",        ""),
    ("extension",  "*.dll",       "ext:dll",     "*.dll",       "dll"),
    ("substring",  "config",      "config",      "config",      ""),
];

// ── Types ────────────────────────────────────────────────────────────────────
#[derive(Clone, Copy, PartialEq, Eq)] enum Tool { Uffs, UffsCpp, Everything }
impl Tool { fn label(self) -> &'static str { match self { Self::Uffs=>"UFFS", Self::UffsCpp=>"UFFS-C++", Self::Everything=>"Everything" } } }

#[derive(Clone, Copy, PartialEq, Eq)] enum Phase { Cold, Warm, Hot }
impl Phase { fn label(self) -> &'static str { match self { Self::Cold=>"COLD", Self::Warm=>"WARM", Self::Hot=>"HOT" } } }

#[derive(Clone, Default)]
struct Timing { wall_ms: u64, daemon_ms: u64, rows: u64, ok: bool, dnf: bool, err: String }

struct Row { tool: Tool, phase: Phase, drive: String, pat: String, runs: Vec<Timing> }
struct Cfg { uffs: PathBuf, uffs_cpp: Option<PathBuf>, es: Option<PathBuf>,
             drives: Vec<String>, rounds: usize, limit: u32,
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


// ── Run: UFFS ────────────────────────────────────────────────────────────────
fn run_uffs(bin: &Path, drive: &str, pattern: &str, limit: u32) -> Timing {
    let t = Instant::now();
    let r = Command::new(bin)
        .args(["search", pattern, "--drive", drive, "--limit", &limit.to_string(), "--profile"])
        .stdout(Stdio::piped()).stderr(Stdio::piped())
        .output();
    let wall = t.elapsed().as_millis() as u64;
    match r {
        Ok(o) if o.status.success() => {
            let out = String::from_utf8_lossy(&o.stdout);
            let err = String::from_utf8_lossy(&o.stderr);
            let rows = out.lines().filter(|l| !l.trim().is_empty() && !l.starts_with("===") && !l.starts_with("---")).count() as u64;
            let dms = parse_daemon_ms(&err);
            Timing { wall_ms: wall, daemon_ms: dms, rows, ok: true, ..Default::default() }
        }
        Ok(o) => Timing { wall_ms: wall, err: String::from_utf8_lossy(&o.stderr).into(), ..Default::default() },
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
fn run_es(bin: &Path, drive: &str, pattern: &str, limit: u32) -> Timing {
    let query = if pattern == "*" { format!("{}:\\", drive) } else { format!("{}:\\ {}", drive, pattern) };
    let t = Instant::now();
    let r = Command::new(bin)
        .args([&query, "-n", &limit.to_string()])
        .stdout(Stdio::piped()).stderr(Stdio::piped())
        .output();
    let wall = t.elapsed().as_millis() as u64;
    match r {
        Ok(o) if o.status.success() => {
            let out = String::from_utf8_lossy(&o.stdout);
            let rows = out.lines().filter(|l| !l.trim().is_empty()).count() as u64;
            Timing { wall_ms: wall, rows, ok: true, ..Default::default() }
        }
        Ok(o) => Timing { wall_ms: wall, err: String::from_utf8_lossy(&o.stderr).into(), ..Default::default() },
        Err(e) => Timing { wall_ms: wall, err: e.to_string(), ..Default::default() },
    }
}

// ── Run: UFFS C++ (uffs.com) ─────────────────────────────────────────────────
/// C++ UFFS reads MFT every invocation (no daemon). No --limit flag.
/// Extension filter uses --ext=<ext> instead of glob *.ext.
/// Substring match needs *needle* glob wildcards.
fn run_uffs_cpp(bin: &Path, drive: &str, pattern: &str, cpp_ext: &str) -> Timing {
    let mut args: Vec<String> = Vec::new();
    // Pattern: if cpp_ext is set, use * as pattern + --ext flag
    // For substring "config", wrap as *config* for glob matching
    if !cpp_ext.is_empty() {
        args.push("*".into());
        args.push(format!("--ext={}", cpp_ext));
    } else if !pattern.contains('*') && !pattern.contains('?') && pattern != "*" {
        // Plain substring like "config" → wrap in glob wildcards
        // But exact names like "notepad.exe" also need wildcards to find in any dir
        args.push(format!("*{}*", pattern));
    } else {
        args.push(pattern.into());
    }
    args.push(format!("--drives={}", drive));
    args.push("--out=console".into());
    let t = Instant::now();
    let r = Command::new(bin)
        .args(&args)
        .stdout(Stdio::piped()).stderr(Stdio::piped())
        .output();
    let wall = t.elapsed().as_millis() as u64;
    match r {
        Ok(o) if o.status.success() => {
            let out = String::from_utf8_lossy(&o.stdout);
            let rows = out.lines().filter(|l| !l.trim().is_empty()).count() as u64;
            Timing { wall_ms: wall, rows, ok: true, ..Default::default() }
        }
        Ok(o) => Timing { wall_ms: wall, err: String::from_utf8_lossy(&o.stderr).into(), ..Default::default() },
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
    let mut limit = DEFAULT_LIMIT;
    let mut tools_str: Option<String> = None;
    let mut skip_cold = false;
    let mut uffs_bin: Option<PathBuf> = None;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--drives" => { i += 1; drives = Some(args[i].split(',').map(|s| s.trim().to_uppercase()).collect()); }
            "--rounds" => { i += 1; rounds = args[i].parse().unwrap_or(DEFAULT_ROUNDS); }
            "--limit"  => { i += 1; limit = args[i].parse().unwrap_or(DEFAULT_LIMIT); }
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
    Cfg { uffs, uffs_cpp, es, drives, rounds, limit, tools, skip_cold }
}

fn print_help() {
    eprintln!("Cross-Tool Benchmark — UFFS (Rust) vs UFFS (C++) vs Everything");
    eprintln!();
    eprintln!("OPTIONS:");
    eprintln!("  --drives C,D        Drives to benchmark (default: C,D)");
    eprintln!("  --rounds 10         Rounds per pattern (default: 10)");
    eprintln!("  --limit 100         Result cap for UFFS Rust & Everything (default: 100)");
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
    println!("║  Limit:        {} rows", cfg.limit);
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

            for &(label, pat, _, _, _) in PATTERNS {
                eprint!("    {label:<12} ");  flush();
                // COLD: only 1 round (destructive — restarts daemon each time)
                uffs_stop(&cfg.uffs);
                uffs_purge_cache();
                let t = check_dnf(run_uffs(&cfg.uffs, drive, pat, cfg.limit));
                let verdict = if t.dnf { "DNF" } else if t.ok { "PASS" } else { "ERROR" };
                eprintln!("{:>8}  {}", fms(t.wall_ms), verdict);
                all_rows.push(Row { tool: Tool::Uffs, phase: Phase::Cold, drive: drive.clone(),
                    pat: label.into(), runs: vec![t] });
            }
        }

        // ── UFFS WARM ────────────────────────────────────────────────────
        if cfg.tools.contains(&Tool::Uffs) && !cfg.skip_cold {
            eprint!("  UFFS WARM: stopping daemon (cache stays)...");  flush();
            uffs_stop(&cfg.uffs);
            eprintln!(" done.");

            for &(label, pat, _, _, _) in PATTERNS {
                eprint!("    {label:<12} ");  flush();
                uffs_stop(&cfg.uffs);
                let t = check_dnf(run_uffs(&cfg.uffs, drive, pat, cfg.limit));
                let verdict = if t.dnf { "DNF" } else if t.ok { "PASS" } else { "ERROR" };
                eprintln!("{:>8}  {}", fms(t.wall_ms), verdict);
                all_rows.push(Row { tool: Tool::Uffs, phase: Phase::Warm, drive: drive.clone(),
                    pat: label.into(), runs: vec![t] });
            }
        }

        // ── UFFS HOT ────────────────────────────────────────────────────
        if cfg.tools.contains(&Tool::Uffs) {
            // Warm up daemon with a throwaway query
            let _ = run_uffs(&cfg.uffs, drive, "*", 1);
            eprintln!("  UFFS HOT:  {} rounds", cfg.rounds);

            for &(label, pat, _, _, _) in PATTERNS {
                eprint!("    {label:<12} ");  flush();
                let mut runs = Vec::new();
                for _ in 0..cfg.rounds {
                    runs.push(check_dnf(run_uffs(&cfg.uffs, drive, pat, cfg.limit)));
                }
                let s = sw(&runs);
                let verdict = if runs.iter().any(|r| r.dnf) { "DNF" } else { "PASS" };
                eprintln!("p50={:>6}  p95={:>6}  {}", fms(p50(&s)), fms(p95(&s)), verdict);
                all_rows.push(Row { tool: Tool::Uffs, phase: Phase::Hot, drive: drive.clone(),
                    pat: label.into(), runs });
            }
        }

        // ── UFFS C++ (uffs.com) ──────────────────────────────────────────
        if cfg.tools.contains(&Tool::UffsCpp) {
            if let Some(ref cpp) = cfg.uffs_cpp {
                eprintln!("  UFFS C++ (MFT re-read, no --limit):  {} rounds", cfg.rounds);
                for &(label, pat, _, _, cpp_ext) in PATTERNS {
                    eprint!("    {label:<12} ");  flush();
                    let mut runs = Vec::new();
                    for _ in 0..cfg.rounds {
                        runs.push(check_dnf(run_uffs_cpp(cpp, drive, pat, cpp_ext)));
                    }
                    let s = sw(&runs);
                    let verdict = if runs.iter().any(|r| r.dnf) { "DNF" } else if runs.iter().all(|r| r.ok) { "PASS" } else { "ERROR" };
                    eprintln!("p50={:>6}  p95={:>6}  {}", fms(p50(&s)), fms(p95(&s)), verdict);
                    all_rows.push(Row { tool: Tool::UffsCpp, phase: Phase::Hot, drive: drive.clone(),
                        pat: label.into(), runs });
                }
            }
        }

        // ── Everything ──────────────────────────────────────────────────
        if cfg.tools.contains(&Tool::Everything) {
            if let Some(ref es) = cfg.es {
                eprintln!("  Everything HOT:  {} rounds (always-hot, daemon model)", cfg.rounds);
                for &(label, _, es_pat, _, _) in PATTERNS {
                    eprint!("    {label:<12} ");  flush();
                    let mut runs = Vec::new();
                    for _ in 0..cfg.rounds {
                        runs.push(check_dnf(run_es(es, drive, es_pat, cfg.limit)));
                    }
                    let s = sw(&runs);
                    let verdict = if runs.iter().any(|r| r.dnf) { "DNF" } else if runs.iter().all(|r| r.ok) { "PASS" } else { "ERROR" };
                    eprintln!("p50={:>6}  p95={:>6}  {}", fms(p50(&s)), fms(p95(&s)), verdict);
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
    println!("| Drive | Tool         | Phase | Pattern      | p50      | p95      | Verdict |");
    println!("|-------|--------------|-------|--------------|----------|----------|---------|");

    for row in rows {
        let s = sw(&row.runs);
        let any_dnf = row.runs.iter().any(|r| r.dnf);
        let all_ok = row.runs.iter().all(|r| r.ok);
        let verdict = if any_dnf { "DNF" } else if all_ok { "PASS" } else { "ERROR" };

        let p50_str = if s.is_empty() { "—".to_string() } else { fms(p50(&s)) };
        let p95_str = if s.is_empty() { "—".to_string() } else { fms(p95(&s)) };

        println!("| {:<5} | {:<12} | {:<5} | {:<12} | {:>8} | {:>8} | {:<7} |",
            format!("{}:", row.drive), row.tool.label(), row.phase.label(),
            row.pat, p50_str, p95_str, verdict);
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
        for &(label, _, _, _, _) in PATTERNS {
            let uffs_p50 = find_p50(rows, Tool::Uffs, Phase::Hot, drive, label);
            let cpp_p50 = find_p50(rows, Tool::UffsCpp, Phase::Hot, drive, label);
            let es_p50 = find_p50(rows, Tool::Everything, Phase::Hot, drive, label);
            println!("| {:<5} | {:<12} | {:>11} | {:>12} | {:>14} |",
                format!("{}:", drive), label, uffs_p50, cpp_p50, es_p50);
        }
    }

    println!();
    println!("Legend:  PASS = completed within {}s.  DNF = timed out.  SKIP = tool not found.", TIMEOUT.as_secs());
    println!("Note:   UFFS (Rust) has three phases: COLD (no cache), WARM (cache), HOT (daemon).");
    println!("        UFFS (C++) re-reads MFT every invocation.  No --limit flag (outputs ALL results).");
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