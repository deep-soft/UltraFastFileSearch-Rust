#!/usr/bin/env rust-script
//! ```cargo
//! [package]
//! edition = "2021"
//! ```

use std::io;
use std::process::{Command, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant};

fn usage() {
    eprintln!("Usage: tui-memory-footprint.rs [--no-cache]");
    eprintln!();
    eprintln!("  --no-cache   Pass --no-cache to uffs_tui (bypass .uffs cache)");
    eprintln!();
    eprintln!("Sanity-check with:");
    eprintln!("  /usr/bin/time -l gtimeout 60s target/release/uffs_tui \\");
    eprintln!("    --data-dir ~/uffs_data --keys windows --reset-history");
}

/// Find the actual uffs_tui process among children of `wrapper_pid`.
/// Uses `pgrep -P <pid> -f uffs_tui` so we skip intermediate shells.
fn find_uffs_pid(wrapper_pid: u32) -> io::Result<Option<u32>> {
    let out = Command::new("/usr/bin/pgrep")
        .args([
            "-P",
            &wrapper_pid.to_string(),
            "-f",
            "target/release/uffs_tui",
        ])
        .output()?;

    if !out.status.success() {
        return Ok(None);
    }

    for line in String::from_utf8_lossy(&out.stdout).lines() {
        if let Ok(pid) = line.trim().parse::<u32>() {
            return Ok(Some(pid));
        }
    }
    Ok(None)
}

/// Sample a PID: returns (state, command, rss_kb) or None if gone.
fn sample_pid(pid: u32) -> io::Result<Option<(String, String, u64)>> {
    let out = Command::new("/bin/ps")
        .args(["-o", "pid=,state=,rss=,command=", "-p", &pid.to_string()])
        .output()?;

    if !out.status.success() {
        return Ok(None);
    }

    let line = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if line.is_empty() {
        return Ok(None);
    }

    let mut parts = line.split_whitespace();
    let _pid = parts.next();
    let state = parts.next().unwrap_or("").to_string();
    let rss_kb = parts.next().unwrap_or("0").parse::<u64>().unwrap_or(0);
    let command = parts.collect::<Vec<_>>().join(" ");

    Ok(Some((state, command, rss_kb)))
}

fn kill_pid(pid: u32) {
    let _ = Command::new("/bin/kill")
        .args(["-TERM", &pid.to_string()])
        .status();
}

fn main() -> io::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let no_cache = args.iter().any(|a| a == "--no-cache");

    if args.iter().any(|a| a == "--help" || a == "-h") {
        usage();
        return Ok(());
    }

    let cmd = if no_cache {
        "target/release/uffs_tui --data-dir ~/uffs_data --keys windows --reset-history --no-cache"
    } else {
        "target/release/uffs_tui --data-dir ~/uffs_data --keys windows --reset-history"
    };

    let mode = if no_cache { "no-cache" } else { "cached" };
    println!("Launching under pseudo-terminal ({mode}):");
    println!("  {cmd}");
    println!();

    let mut wrapper = Command::new("/usr/bin/script")
        .args([
            "-q",
            "/dev/null",
            "/bin/sh",
            "-lc",
            &format!("exec {cmd}"),
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;

    let wrapper_pid = wrapper.id();

    // Poll until the real uffs_tui process appears (not an intermediate shell)
    let start = Instant::now();
    let target_pid = loop {
        if let Some(pid) = find_uffs_pid(wrapper_pid)? {
            break pid;
        }
        if start.elapsed() > Duration::from_secs(5) {
            eprintln!("ERROR: could not find uffs_tui child of wrapper pid {wrapper_pid}");
            kill_pid(wrapper_pid);
            let _ = wrapper.wait();
            std::process::exit(1);
        }
        sleep(Duration::from_millis(100));
    };

    println!("wrapper_pid={wrapper_pid}");
    println!("target_pid={target_pid}");
    println!();
    println!("sec,state,rss_kb,rss_mib,command");

    let mut max_rss_kb = 0u64;
    let mut min_rss_kb = u64::MAX;
    let mut last_rss_kb = 0u64;

    for sec in 1..=60 {
        match sample_pid(target_pid)? {
            Some((state, command, rss_kb)) => {
                if rss_kb > max_rss_kb {
                    max_rss_kb = rss_kb;
                }
                if rss_kb > 0 && rss_kb < min_rss_kb {
                    min_rss_kb = rss_kb;
                }
                last_rss_kb = rss_kb;
                println!(
                    "{sec},{state},{rss_kb},{:.2},{command}",
                    rss_kb as f64 / 1024.0,
                );
            }
            None => {
                println!("{sec},<gone>,0,0.00,<process exited>");
                break;
            }
        }
        sleep(Duration::from_secs(1));
    }

    kill_pid(target_pid);
    kill_pid(wrapper_pid);
    let _ = wrapper.wait();

    println!();
    if max_rss_kb == 0 {
        println!("No RSS samples collected.");
    } else {
        println!("Summary ({mode}):");
        println!("  min_rss_kb  = {min_rss_kb}");
        println!("  max_rss_kb  = {max_rss_kb}");
        println!("  last_rss_kb = {last_rss_kb}");
        println!("  min_rss_mib = {:.2}", min_rss_kb as f64 / 1024.0);
        println!("  max_rss_mib = {:.2}", max_rss_kb as f64 / 1024.0);
        println!("  last_rss_mib = {:.2}", last_rss_kb as f64 / 1024.0);
    }

    Ok(())
}
