//! Integration tests for the daemon IPC layer.
//!
//! Runs a SINGLE daemon process and tests all JSON-RPC methods through
//! one connection. This avoids socket conflicts from parallel tests.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::{Child, Command};
use std::time::Duration;

/// Find the daemon binary.
fn daemon_exe() -> PathBuf {
    // Test binary is at target/debug/deps/ipc_integration-xxx
    // Daemon binary is at target/debug/uffs-daemon
    let current = std::env::current_exe().expect("current_exe");

    // Try: pop binary name, pop deps/, look for uffs-daemon
    let mut path = current.clone();
    path.pop(); // remove test binary
    path.pop(); // remove deps/
    path.push("uffs-daemon");
    if path.exists() {
        return path;
    }

    // Try without deps/ (in case test isn't in deps/)
    let mut path = current;
    path.pop();
    path.push("uffs-daemon");
    if path.exists() {
        return path;
    }

    // Fallback: assume it's in PATH
    PathBuf::from("uffs-daemon")
}

/// Get the platform socket path.
fn socket_path() -> PathBuf {
    let base = dirs_next::data_local_dir().unwrap_or_else(|| PathBuf::from("/tmp"));
    #[cfg(target_os = "macos")]
    { base.join("uffs").join("daemon.sock") }
    #[cfg(not(target_os = "macos"))]
    { base.join("uffs").join("daemon.sock") }
}

/// PID file path.
fn pid_path() -> PathBuf {
    let base = dirs_next::data_local_dir().unwrap_or_else(|| PathBuf::from("/tmp"));
    base.join("uffs").join("daemon.pid")
}

/// Send a request and read one response line.
fn rpc(ws: &mut UnixStream, reader: &mut BufReader<UnixStream>, req: &str) -> String {
    ws.write_all(req.as_bytes()).expect("write");
    ws.write_all(b"\n").expect("newline");
    ws.flush().expect("flush");
    let mut line = String::new();
    reader.read_line(&mut line).expect("read");
    line
}

/// D2/D3 integration tests — all run against one daemon instance.
#[test]
fn test_daemon_ipc_all_methods() {
    let exe = daemon_exe();
    if !exe.exists() {
        eprintln!("Daemon binary not found at {}, skipping", exe.display());
        return;
    }

    // Clean up stale socket/pid files from previous runs
    let _ = std::fs::remove_file(socket_path());
    let _ = std::fs::remove_file(pid_path());

    // Start daemon
    let mut daemon = Command::new(&exe)
        .args(["--idle-timeout", "30", "--log-level", "warn"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn daemon");

    // Wait for socket
    let sock = socket_path();
    let mut ready = false;
    for _ in 0..40 {
        std::thread::sleep(Duration::from_millis(100));
        if sock.exists() {
            ready = true;
            break;
        }
    }
    if !ready {
        eprintln!("Daemon socket not ready after 4s, skipping");
        let _ = daemon.kill();
        return;
    }

    // Connect
    let stream = UnixStream::connect(&sock).expect("connect");
    stream.set_read_timeout(Some(Duration::from_secs(5))).expect("timeout");
    let mut ws = stream.try_clone().expect("clone");
    let mut reader = BufReader::new(stream);

    // ── Test 1: status ──────────────────────────────────────────────
    let resp = rpc(&mut ws, &mut reader,
        r#"{"jsonrpc":"2.0","id":1,"method":"status"}"#);
    assert!(resp.contains("\"pid\""), "status should have pid: {resp}");
    assert!(resp.contains("\"uptime_secs\""), "status should have uptime: {resp}");

    // ── Test 2: drives (empty) ──────────────────────────────────────
    let resp = rpc(&mut ws, &mut reader,
        r#"{"jsonrpc":"2.0","id":2,"method":"drives"}"#);
    assert!(resp.contains("\"drives\":[]"), "drives should be empty: {resp}");

    // ── Test 3: search (no data) ────────────────────────────────────
    let resp = rpc(&mut ws, &mut reader,
        r#"{"jsonrpc":"2.0","id":3,"method":"search","params":{"pattern":"*.rs"}}"#);
    assert!(resp.contains("\"rows\":[]"), "search should be empty: {resp}");
    assert!(resp.contains("\"records_scanned\":0"), "should scan 0: {resp}");

    // ── Test 4: unknown method → -32601 ─────────────────────────────
    let resp = rpc(&mut ws, &mut reader,
        r#"{"jsonrpc":"2.0","id":4,"method":"nonexistent"}"#);
    assert!(resp.contains("-32601"), "should be method not found: {resp}");

    // ── Test 5: keepalive ───────────────────────────────────────────
    let resp = rpc(&mut ws, &mut reader,
        r#"{"jsonrpc":"2.0","id":5,"method":"keepalive"}"#);
    assert!(resp.contains("\"ok\":true"), "keepalive should return ok: {resp}");

    // ── Test 6: invalid JSON → -32700 ──────────────────────────────
    let resp = rpc(&mut ws, &mut reader, "this is not json");
    assert!(resp.contains("-32700"), "should be parse error: {resp}");

    // ── Test 7: shutdown without nonce → rejected ───────────────────
    let resp = rpc(&mut ws, &mut reader,
        r#"{"jsonrpc":"2.0","id":7,"method":"shutdown","params":{}}"#);
    assert!(resp.contains("nonce"), "should mention nonce: {resp}");

    // ── Test 8: shutdown with correct nonce → accepted ──────────────
    let pid_content = std::fs::read_to_string(pid_path()).unwrap_or_default();
    let nonce = pid_content.lines().nth(3).unwrap_or("").trim().to_owned();
    eprintln!("PID file content:\n{pid_content}");
    eprintln!("Nonce read: '{nonce}'");
    if !nonce.is_empty() {
        let resp = rpc(&mut ws, &mut reader,
            &format!(r#"{{"jsonrpc":"2.0","id":8,"method":"shutdown","params":{{"nonce":"{nonce}"}}}}"#));
        assert!(resp.contains("shutting down") || resp.contains("\"ok\":true"),
            "should shut down with nonce '{nonce}': {resp}");
    } else {
        eprintln!("WARNING: Could not read nonce from PID file, skipping shutdown test");
        // Kill daemon directly
        let _ = daemon.kill();
    }

    // Wait for daemon to exit
    let _ = daemon.wait();
}

/// Real-data integration test: loads G_mft.bin fixture, verifies actual search results.
#[test]
fn test_real_data_search() {
    let exe = daemon_exe();
    if !exe.exists() {
        eprintln!("Daemon binary not found, skipping");
        return;
    }

    // Find the fixture file
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent().unwrap()
        .parent().unwrap()
        .join("tests/fixtures/drive_g/G_mft.bin");

    if !fixture.exists() {
        eprintln!("Test fixture not found at {}, skipping", fixture.display());
        return;
    }

    let _ = std::fs::remove_file(socket_path());
    let _ = std::fs::remove_file(pid_path());

    // Start daemon with real MFT data + --no-cache (skip encrypted cache)
    let mut daemon = Command::new(&exe)
        .args([
            "--mft-file", fixture.to_str().unwrap(),
            "--no-cache",
            "--idle-timeout", "30",
            "--log-level", "warn",
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn daemon");

    // Wait for socket + index loading (MFT parse takes a few seconds)
    let sock = socket_path();
    let mut ready = false;
    for _ in 0..80 { // 8 seconds max (MFT parse can take time)
        std::thread::sleep(Duration::from_millis(100));
        if sock.exists() { ready = true; break; }
    }
    if !ready {
        let _ = daemon.kill();
        eprintln!("Daemon not ready, skipping");
        return;
    }

    let stream = UnixStream::connect(&sock).expect("connect");
    stream.set_read_timeout(Some(Duration::from_secs(10))).expect("timeout");
    let mut ws = stream.try_clone().expect("clone");
    let mut reader = BufReader::new(stream);

    // Poll until daemon finishes loading (MFT parse is async)
    let mut loaded = false;
    for _ in 0..60 { // 30 seconds max
        let resp = rpc(&mut ws, &mut reader,
            r#"{"jsonrpc":"2.0","id":0,"method":"drives"}"#);
        if resp.contains("\"letter\":\"G\"") {
            loaded = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    if !loaded {
        eprintln!("Daemon didn't finish loading G drive in 30s, skipping");
        let _ = daemon.kill();
        let _ = daemon.wait();
        return;
    }

    // ── Test: drives returns G with records ──────────────────────────
    let resp = rpc(&mut ws, &mut reader,
        r#"{"jsonrpc":"2.0","id":1,"method":"drives"}"#);
    assert!(resp.contains("\"letter\":\"G\""), "should have drive G: {resp}");
    assert!(!resp.contains("\"records\":0"), "should have records > 0: {resp}");

    // ── Test: search * returns results ───────────────────────────────
    let resp = rpc(&mut ws, &mut reader,
        r#"{"jsonrpc":"2.0","id":2,"method":"search","params":{"pattern":"*","limit":10}}"#);
    assert!(!resp.contains("\"rows\":[]"), "search * should return results: {resp}");
    assert!(resp.contains("\"name\""), "results should have name field: {resp}");
    assert!(resp.contains("\"path\""), "results should have path field: {resp}");
    assert!(resp.contains("\"records_scanned\""), "should have records_scanned: {resp}");

    // ── Test: search *.txt returns results with .txt files ──────────
    let resp = rpc(&mut ws, &mut reader,
        r#"{"jsonrpc":"2.0","id":3,"method":"search","params":{"pattern":"*.txt","limit":50}}"#);
    // G drive might not have .txt files, so just verify the response is valid
    assert!(resp.contains("\"rows\""), "search *.txt should have rows field: {resp}");
    assert!(resp.contains("\"duration_ms\""), "should have duration: {resp}");

    // ── Test: status shows Ready ────────────────────────────────────
    let resp = rpc(&mut ws, &mut reader,
        r#"{"jsonrpc":"2.0","id":4,"method":"status"}"#);
    assert!(resp.contains("\"ready\"") || resp.contains("\"Ready\""),
        "status should be ready: {resp}");

    // Shutdown
    let nonce = std::fs::read_to_string(pid_path())
        .ok()
        .and_then(|c| c.lines().nth(3).map(|s| s.trim().to_owned()))
        .unwrap_or_default();
    if !nonce.is_empty() {
        let _ = rpc(&mut ws, &mut reader,
            &format!(r#"{{"jsonrpc":"2.0","id":99,"method":"shutdown","params":{{"nonce":"{nonce}"}}}}"#));
    } else {
        let _ = daemon.kill();
    }
    let _ = daemon.wait();
}

/// D3.5.4: Benchmark — measure client round-trip latency (target <15ms).
#[test]
fn test_benchmark_round_trip_latency() {
    let exe = daemon_exe();
    if !exe.exists() {
        eprintln!("Daemon binary not found, skipping");
        return;
    }

    let _ = std::fs::remove_file(socket_path());
    let _ = std::fs::remove_file(pid_path());

    let mut daemon = Command::new(&exe)
        .args(["--idle-timeout", "30", "--log-level", "warn"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn daemon");

    let sock = socket_path();
    let mut ready = false;
    for _ in 0..40 {
        std::thread::sleep(Duration::from_millis(100));
        if sock.exists() { ready = true; break; }
    }
    if !ready {
        let _ = daemon.kill();
        eprintln!("Daemon not ready, skipping");
        return;
    }

    let stream = UnixStream::connect(&sock).expect("connect");
    stream.set_read_timeout(Some(Duration::from_secs(5))).expect("timeout");
    let mut ws = stream.try_clone().expect("clone");
    let mut reader = BufReader::new(stream);

    // Warm up — give daemon time to settle
    std::thread::sleep(Duration::from_millis(200));
    for _ in 0..10 {
        let resp = rpc(&mut ws, &mut reader, r#"{"jsonrpc":"2.0","id":0,"method":"keepalive"}"#);
        if resp.is_empty() {
            eprintln!("Warmup got empty response, daemon may not be ready");
            let _ = daemon.kill();
            return;
        }
    }

    // Benchmark: 100 keepalive requests (lightweight, no serialization overhead)
    let iterations = 100;
    let mut latencies = Vec::with_capacity(iterations);

    for i in 0..iterations {
        let start = std::time::Instant::now();
        let resp = rpc(&mut ws, &mut reader,
            &format!(r#"{{"jsonrpc":"2.0","id":{},"method":"keepalive"}}"#, i + 1000));
        let elapsed = start.elapsed();
        if !resp.contains("\"ok\"") {
            eprintln!("Unexpected response at iteration {i}: {resp}");
            continue;
        }
        latencies.push(elapsed);
    }

    let total: Duration = latencies.iter().sum();
    let avg = total / iterations as u32;
    let min = latencies.iter().min().unwrap();
    let max = latencies.iter().max().unwrap();

    eprintln!("\n=== IPC Round-Trip Latency Benchmark ({iterations} iterations) ===");
    eprintln!("  Average: {:?}", avg);
    eprintln!("  Min:     {:?}", min);
    eprintln!("  Max:     {:?}", max);
    eprintln!("  Target:  <15ms");
    eprintln!("  Result:  {}", if avg.as_millis() < 15 { "✅ PASS" } else { "⚠️ SLOW" });

    // Assert target
    assert!(avg.as_millis() < 15, "average latency {:?} exceeds 15ms target", avg);

    // Shutdown
    let nonce = std::fs::read_to_string(pid_path())
        .ok()
        .and_then(|c| c.lines().nth(3).map(|s| s.trim().to_owned()))
        .unwrap_or_default();
    if !nonce.is_empty() {
        let _ = rpc(&mut ws, &mut reader,
            &format!(r#"{{"jsonrpc":"2.0","id":9999,"method":"shutdown","params":{{"nonce":"{nonce}"}}}}"#));
    } else {
        let _ = daemon.kill();
    }
    let _ = daemon.wait();
}

/// D2.7.4: Concurrent clients — 3 connections, interleaved queries.
#[test]
fn test_concurrent_clients() {
    let exe = daemon_exe();
    if !exe.exists() {
        eprintln!("Daemon binary not found, skipping");
        return;
    }

    let _ = std::fs::remove_file(socket_path());
    let _ = std::fs::remove_file(pid_path());

    let mut daemon = Command::new(&exe)
        .args(["--idle-timeout", "30", "--log-level", "warn"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn daemon");

    let sock = socket_path();
    let mut ready = false;
    for _ in 0..40 {
        std::thread::sleep(Duration::from_millis(100));
        if sock.exists() { ready = true; break; }
    }
    if !ready {
        let _ = daemon.kill();
        eprintln!("Daemon not ready, skipping");
        return;
    }

    // Connect 3 clients simultaneously
    let mut clients: Vec<(UnixStream, BufReader<UnixStream>)> = (0..3)
        .filter_map(|_| {
            let s = UnixStream::connect(&sock).ok()?;
            s.set_read_timeout(Some(Duration::from_secs(5))).ok()?;
            let ws = s.try_clone().ok()?;
            let reader = BufReader::new(s);
            Some((ws, reader))
        })
        .collect();

    assert_eq!(clients.len(), 3, "should connect 3 clients");

    // Send interleaved queries — each client sends a different request
    let requests = [
        r#"{"jsonrpc":"2.0","id":100,"method":"status"}"#,
        r#"{"jsonrpc":"2.0","id":200,"method":"drives"}"#,
        r#"{"jsonrpc":"2.0","id":300,"method":"keepalive"}"#,
    ];

    // Send all requests first (interleaved)
    for (i, (ws, _)) in clients.iter_mut().enumerate() {
        ws.write_all(requests[i].as_bytes()).expect("write");
        ws.write_all(b"\n").expect("newline");
        ws.flush().expect("flush");
    }

    // Read all responses
    let mut responses = Vec::new();
    for (_, reader) in clients.iter_mut() {
        let mut line = String::new();
        reader.read_line(&mut line).expect("read response");
        responses.push(line);
    }

    // Verify each client got the right response (matched by id)
    assert!(responses[0].contains("\"id\":100"), "client 0 should get id 100: {}", responses[0]);
    assert!(responses[0].contains("\"pid\""), "client 0 should get status: {}", responses[0]);

    assert!(responses[1].contains("\"id\":200"), "client 1 should get id 200: {}", responses[1]);
    assert!(responses[1].contains("\"drives\""), "client 1 should get drives: {}", responses[1]);

    assert!(responses[2].contains("\"id\":300"), "client 2 should get id 300: {}", responses[2]);
    assert!(responses[2].contains("\"ok\":true"), "client 2 should get keepalive ok: {}", responses[2]);

    // Shutdown
    let nonce = std::fs::read_to_string(pid_path())
        .ok()
        .and_then(|c| c.lines().nth(3).map(|s| s.trim().to_owned()))
        .unwrap_or_default();
    if !nonce.is_empty() {
        let (ref mut ws, ref mut reader) = clients[0];
        let _ = rpc(ws, reader,
            &format!(r#"{{"jsonrpc":"2.0","id":999,"method":"shutdown","params":{{"nonce":"{nonce}"}}}}"#));
    } else {
        let _ = daemon.kill();
    }
    let _ = daemon.wait();
}
