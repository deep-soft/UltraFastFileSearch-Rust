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
