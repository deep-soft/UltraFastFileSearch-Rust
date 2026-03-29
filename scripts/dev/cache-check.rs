#!/usr/bin/env rust-script
//! Cross-platform cache diagnostic for UFFS.
//! Replicates `scripts/windows/cache-check.ps1` as a rust-script (macOS/Linux/Windows).
//! Builds the release binary, clears cache, runs N search rounds, reports timing + cache ages.
//!
//! ```bash
//! rust-script scripts/dev/cache-check.rs --file ~/uffs_data/D_mft.raw -r 3
//! rust-script scripts/dev/cache-check.rs --drive D -r 3          # Windows live
//! rust-script scripts/dev/cache-check.rs --file D_mft.raw --no-build
//! ```
//! ```cargo
//! [dependencies]
//! dirs-next = "2.0"
//! regex = "1"
//! ```
use std::{env, fs};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Instant, SystemTime};

fn cache_dir() -> PathBuf {
    let b = dirs_next::cache_dir().unwrap_or_else(|| env::temp_dir());
    if cfg!(target_os = "macos") { b.join("com.uffs") }
    else if cfg!(target_os = "windows") { b.join("uffs").join("cache") }
    else { b.join("uffs") }
}
fn cache_path(d: char, s: &str) -> PathBuf { cache_dir().join(format!("{}_{s}.uffs", d.to_ascii_uppercase())) }
fn age_s(p: &Path) -> Option<u64> { fs::metadata(p).ok()?.modified().ok().and_then(|m| SystemTime::now().duration_since(m).ok()).map(|d| d.as_secs()) }
fn sz_mb(p: &Path) -> f64 { fs::metadata(p).map(|m| m.len() as f64 / 1_048_576.0).unwrap_or(0.0) }
fn ws_root() -> PathBuf {
    let c = env::current_dir().unwrap_or_else(|_| ".".into());
    let mut d = c.as_path();
    loop { if d.join("Cargo.toml").exists() && d.join(".cargo").exists() { return d.into(); } match d.parent() { Some(p) => d = p, None => return c } }
}
fn target_dir(ws: &Path) -> PathBuf {
    let o = Command::new("cargo").args(["metadata","--no-deps","--format-version","1"]).current_dir(ws).output().expect("cargo metadata");
    let s = String::from_utf8_lossy(&o.stdout);
    let n = "\"target_directory\":\"";
    if let Some(i) = s.find(n) {
        let (mut r, mut esc) = (String::new(), false);
        for ch in s[i+n.len()..].chars() { if esc { r.push(ch); esc=false; } else if ch=='\\' { esc=true; } else if ch=='"' { break; } else { r.push(ch); } }
        return r.into();
    }
    ws.join("target")
}
fn bin_name() -> &'static str { if cfg!(windows) { "uffs.exe" } else { "uffs" } }
fn build(ws: &Path) {
    println!("╔══════════════════════════════════════════════════════════════╗");
    println!("║  Building release binary (cargo build --release)...         ║");
    println!("╚══════════════════════════════════════════════════════════════╝");
    let t = Instant::now();
    let s = Command::new("cargo").args(["build","--release","-p","uffs-cli","--bin","uffs"]).current_dir(ws).status().expect("build");
    if !s.success() { eprintln!("ERROR: build failed"); std::process::exit(1); }
    println!("✅ Built in {:.1}s\n", t.elapsed().as_secs_f64());
}
fn show_cache() {
    let d = cache_dir();
    println!("\n─── Cache Status ───");
    println!("  Cache dir: {}", d.display());
    if !d.is_dir() { println!("    (does not exist)\n"); return; }
    let mut e: Vec<_> = fs::read_dir(&d).into_iter().flatten().filter_map(|e| e.ok()).filter(|e| e.path().extension().map_or(false, |x| x=="uffs")).collect();
    e.sort_by_key(|e| e.file_name());
    if e.is_empty() { println!("    (no .uffs files)"); }
    for f in &e { let p=f.path(); let a=age_s(&p).unwrap_or(0); let t=if a<600 {"✅"} else {"⏰"}; println!("    {}: {:.2} MB, age={a}s {t}", f.file_name().to_string_lossy(), sz_mb(&p)); }
    println!();
}
struct R { ms: u128, rows: u64, prof: Vec<String> }
fn run(bin: &Path, args: &[&str]) -> R {
    let t = Instant::now();
    let mut c = Command::new(bin).args(args).env("UFFS_CACHE_PROFILE","1").stdout(Stdio::null()).stderr(Stdio::piped()).spawn().unwrap_or_else(|e| { eprintln!("ERROR: {e}"); std::process::exit(1); });
    let re = regex::Regex::new(r"\((\d+) rows?\)").unwrap();
    let (mut prof, mut rows) = (Vec::new(), 0u64);
    for l in BufReader::new(c.stderr.take().unwrap()).lines().flatten() {
        if !l.starts_with("[CACHE_PROFILE]") { continue; }
        if l.contains("row_output:") { if let Some(c) = re.captures(&l) { rows = c[1].parse().unwrap_or(0); } }
        prof.push(l.trim_start_matches("[CACHE_PROFILE]").trim().into());
    }
    let s = c.wait().expect("wait"); if !s.success() { eprintln!("  ⚠ exit {s}"); }
    R { ms: t.elapsed().as_millis(), rows, prof }
}
fn main() {
    let a: Vec<String> = env::args().collect();
    let (mut drv, mut file, mut rounds, mut nb) = ('C', None::<PathBuf>, 3usize, false);
    let mut i = 1;
    while i < a.len() {
        match a[i].as_str() {
            "--drive"|"-d" => { i+=1; drv = a.get(i).and_then(|s| s.chars().next()).unwrap_or('C').to_ascii_uppercase(); }
            "--file"|"-f"  => { i+=1; file = a.get(i).map(PathBuf::from); }
            "--rounds"|"-r"=> { i+=1; rounds = a.get(i).and_then(|s| s.parse().ok()).unwrap_or(3); }
            "--no-build"   => nb = true,
            "--help"|"-h"  => { println!("Usage: rust-script scripts/dev/cache-check.rs [OPTIONS]\n  --drive/-d <L>  Drive letter (Windows live)\n  --file/-f <P>   MFT/index file path (offline, cross-platform)\n  --rounds/-r <N> Rounds (default 3)\n  --no-build      Skip build"); std::process::exit(0); }
            s => { let p = PathBuf::from(s); if p.exists() { file = Some(p); } }
        }
        i += 1;
    }
    if let Some(ref f) = file {
        if drv == 'C' { if let Some(ch) = f.file_name().and_then(|n| n.to_str()).and_then(|n| n.chars().next()) { if ch.is_ascii_alphabetic() { drv = ch.to_ascii_uppercase(); } } }
    }
    let (sa, nca) = if let Some(ref p) = file {
        let ps = p.to_string_lossy().to_string();
        // --mft-file for .raw files, --index for .uffs pre-built indexes
        let flag = if ps.ends_with(".raw") || ps.ends_with(".iocp") { "--mft-file" } else { "--index" };
        (vec!["*".into(), flag.into(), ps.clone()], vec!["*".into(), flag.into(), ps, "--no-cache".into()])
    } else {
        let ds = drv.to_string();
        (vec!["*".into(), "--drive".into(), ds.clone()], vec!["*".into(), "--drive".into(), ds, "--no-cache".into()])
    };
    let ws = ws_root();
    println!("╔════════════════════════════════════════╗");
    println!("║   UFFS Cache Diagnostic — Drive {drv}      ║");
    println!("╚════════════════════════════════════════╝");
    if let Some(ref p) = file { println!("  Mode: File {}", p.display()); } else { println!("  Mode: Live drive {drv}"); }
    if !nb { build(&ws); }
    let bin = target_dir(&ws).join("release").join(bin_name());
    if !bin.exists() { eprintln!("ERROR: Binary not found: {}\n  Build first or use --no-build with pre-built binary.", bin.display()); std::process::exit(1); }
    let ver = Command::new(&bin).arg("--version").output().map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).unwrap_or_else(|_| "?".into());
    println!("  Binary: {}\n  Version: {ver}", bin.display());
    // Step 1: clear
    println!("\n[STEP 1] Clearing cache for drive {drv}:...");
    for s in &["index","compact"] { let p = cache_path(drv, s); if p.is_dir() { let _ = fs::remove_dir_all(&p); println!("  Removed dir {}", p.display()); } else if p.exists() { let _ = fs::remove_file(&p); println!("  Removed {}", p.display()); } }
    // Step 2
    println!("\n[STEP 2] Cache state (post-clear):"); show_cache();
    // Step 3: rounds
    println!("[STEP 3] Running {rounds} searches:");
    let mut times = Vec::new();
    let refs: Vec<&str> = sa.iter().map(|s| s.as_str()).collect();
    for n in 1..=rounds {
        let lbl = if n==1 { "COLD (no cache)".into() } else { format!("RUN {n} (should use cache)") };
        println!("\n  ── Run {n} ({lbl}) ──");
        let r = run(&bin, &refs);
        times.push(r.ms);
        let rl = if r.rows > 0 { format!("{} rows", r.rows) } else { "rows N/A".into() };
        println!("     Time: {} ms ({rl})", r.ms);
        for l in &r.prof { println!("     ⏱  {l}"); }
        let ip = cache_path(drv, "index");
        if ip.exists() { println!("     Index cache:   {:.2} MB, age={}s ✅", sz_mb(&ip), age_s(&ip).unwrap_or(0)); }
        let cp = cache_path(drv, "compact");
        if cp.exists() && !cp.is_dir() { println!("     Compact cache: {:.2} MB, age={}s ✅", sz_mb(&cp), age_s(&cp).unwrap_or(0)); }
        else if cp.is_dir() { println!("     Compact cache: ⚠ IS A DIRECTORY (bug!)"); }
    }
    // Step 4
    println!("\n[STEP 4] Final cache state:"); show_cache();
    // Step 5
    println!("[STEP 5] Running with --no-cache:");
    let nrefs: Vec<&str> = nca.iter().map(|s| s.as_str()).collect();
    let nc = run(&bin, &nrefs);
    let ncl = if nc.rows > 0 { format!("{} rows", nc.rows) } else { "rows N/A".into() };
    println!("     Time: {} ms ({ncl})", nc.ms);
    for l in &nc.prof { println!("     ⏱  {l}"); }
    // Summary
    println!("\n─── Summary ───");
    let cold = times.first().copied().unwrap_or(0);
    let cavg = if times.len() > 1 { times[1..].iter().sum::<u128>() / (times.len()-1) as u128 } else { 0 };
    let sp = if cavg > 0 { cold as f64 / cavg as f64 } else { 0.0 };
    println!("  Cold (Run 1):    {cold} ms");
    println!("  Cached (avg):    {cavg} ms");
    println!("  No-cache:        {} ms", nc.ms);
    println!("  Speedup:         {sp:.1}x (cache vs cold)");
    if sp < 1.5 { println!("\n⚠️  Cache NOT providing significant speedup."); }
    else if sp < 3.0 { println!("\n✅ Cache working. Bottleneck likely deserialization or output."); }
    else { println!("\n✅ Cache working well!"); }
}

