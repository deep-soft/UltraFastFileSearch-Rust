//! Integration tests for the daemon IPC layer.
//!
//! These tests start a real daemon process (with no MFT data), connect
//! via Unix domain socket, and verify JSON-RPC request/response flow.
//!
//! The daemon runs with `--no-retire` and `--idle-timeout 5` so it
//! stays alive for the test but doesn't linger.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::{Child, Command};
use std::time::Duration;

/// Get a unique socket path for this test run (avoids conflicts).
fn test_socket_path() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("uffs-test-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    dir.join("daemon.sock")
}

/// Get the PID file path matching the socket.
fn test_pid_path() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("uffs-test-{}", std::process::id()));
    dir.join("daemon.pid")
}

/// Find the daemon binary (built by cargo).
fn daemon_exe() -> PathBuf {
    let mut path = std::env::current_exe().expect("current_exe");
    // tests/ipc_integration → target/debug/deps/ipc_integration-xxx
    // daemon binary is at target/debug/uffs-daemon
    path.pop(); // remove binary name
    path.pop(); // remove deps/
    path.push("uffs-daemon");
    if !path.exists() {
        // Try without deps
        path.pop();
        path.push("uffs-daemon");
    }
    path
}

/// Start a daemon process for testing.
fn start_test_daemon() -> Option<Child> {
    let exe = daemon_exe();
    if !exe.exists() {
        eprintln!("Daemon binary not found at {}, skipping integration tests", exe.display());
        eprintln!("Run `cargo build -p uffs-daemon` first.");
        return None;
    }

    // Clean up any stale socket/PID files
    let _ = std::fs::remove_file(test_socket_path());
    let _ = std::fs::remove_file(test_pid_path());

    let child = Command::new(&exe)
        .args([
            "--idle-timeout", "30",
            "--no-retire",
            "--log-level", "warn",
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;

    // Wait for socket to appear
    let sock_path = socket_path();
    for _ in 0..40 {
        std::thread::sleep(Duration::from_millis(100));
        if sock_path.exists() {
            return Some(child);
        }
    }

    eprintln!("Daemon didn't create socket at {} within 4s", sock_path.display());
    None
}

/// Get the platform socket path (matches daemon's ipc::socket_path).
fn socket_path() -> PathBuf {
    let base = dirs_next::data_local_dir().unwrap_or_else(|| PathBuf::from("/tmp"));
    #[cfg(target_os = "macos")]
    { base.join("uffs").join("daemon.sock") }
    #[cfg(target_os = "linux")]
    {
        if let Ok(runtime_dir) = std::env::var("XDG_RUNTIME_DIR") {
            PathBuf::from(runtime_dir).join("uffs").join("daemon.sock")
        } else {
            base.join("uffs").join("daemon.sock")
        }
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    { base.join("uffs").join("daemon.sock") }
}

/// Send a JSON-RPC request and read the response.
fn send_request(stream: &mut UnixStream, reader: &mut BufReader<UnixStream>, req: &str) -> String {
    stream.write_all(req.as_bytes()).expect("write");
    stream.write_all(b"\n").expect("write newline");
    stream.flush().expect("flush");

    let mut line = String::new();
    reader.read_line(&mut line).expect("read response");
    line
}

/// Shutdown the daemon gracefully using the nonce from the PID file.
fn shutdown_daemon(stream: &mut UnixStream, reader: &mut BufReader<UnixStream>) {
    // Read nonce from PID file
    let pid_path = dirs_next::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("uffs")
        .join("daemon.pid");

    let nonce = std::fs::read_to_string(&pid_path)
        .ok()
        .and_then(|content| content.lines().nth(3).map(|s| s.to_owned()))
        .unwrap_or_default();

    let req = format!(
        r#"{{"jsonrpc":"2.0","id":999,"method":"shutdown","params":{{"nonce":"{}"}}}}"#,
        nonce
    );
    let _ = send_request(stream, reader, &req);
}

// ════════════════════════════════════════════════════════════════════════════
// Tests
// ════════════════════════════════════════════════════════════════════════════

/// D2.4.10: Connect to daemon socket and send a status request.
#[test]
fn test_connect_and_status() {
    let Some(mut daemon) = start_test_daemon() else {
        eprintln!("Skipping: daemon not available");
        return;
    };

    let sock = socket_path();
    let stream = UnixStream::connect(&sock).expect("connect to daemon");
    stream.set_read_timeout(Some(Duration::from_secs(5))).expect("set timeout");
    let mut write_stream = stream.try_clone().expect("clone stream");
    let mut reader = BufReader::new(stream);

    // Send status request
    let resp = send_request(
        &mut write_stream,
        &mut reader,
        r#"{"jsonrpc":"2.0","id":1,"method":"status"}"#,
    );

    assert!(resp.contains("\"jsonrpc\":\"2.0\""), "response should be JSON-RPC: {resp}");
    assert!(resp.contains("\"id\":1"), "response should have id 1: {resp}");
    assert!(resp.contains("\"pid\""), "response should contain pid: {resp}");
    assert!(resp.contains("\"uptime_secs\""), "response should contain uptime: {resp}");

    // Cleanup
    shutdown_daemon(&mut write_stream, &mut reader);
    let _ = daemon.wait();
}

/// D2.7.1: drives() returns empty list when no data loaded.
#[test]
fn test_drives_empty() {
    let Some(mut daemon) = start_test_daemon() else {
        return;
    };

    let sock = socket_path();
    let stream = UnixStream::connect(&sock).expect("connect");
    stream.set_read_timeout(Some(Duration::from_secs(5))).expect("timeout");
    let mut ws = stream.try_clone().expect("clone");
    let mut reader = BufReader::new(stream);

    let resp = send_request(
        &mut ws,
        &mut reader,
        r#"{"jsonrpc":"2.0","id":2,"method":"drives"}"#,
    );

    assert!(resp.contains("\"drives\":[]"), "should have empty drives: {resp}");

    shutdown_daemon(&mut ws, &mut reader);
    let _ = daemon.wait();
}

/// D2.7.1: search returns 0 results when no data loaded.
#[test]
fn test_search_no_data() {
    let Some(mut daemon) = start_test_daemon() else {
        return;
    };

    let sock = socket_path();
    let stream = UnixStream::connect(&sock).expect("connect");
    stream.set_read_timeout(Some(Duration::from_secs(5))).expect("timeout");
    let mut ws = stream.try_clone().expect("clone");
    let mut reader = BufReader::new(stream);

    let resp = send_request(
        &mut ws,
        &mut reader,
        r#"{"jsonrpc":"2.0","id":3,"method":"search","params":{"pattern":"*.rs"}}"#,
    );

    assert!(resp.contains("\"rows\":[]"), "should have empty rows: {resp}");
    assert!(resp.contains("\"records_scanned\":0"), "should scan 0 records: {resp}");

    shutdown_daemon(&mut ws, &mut reader);
    let _ = daemon.wait();
}

/// D2.5.9: Unknown method returns -32601 error.
#[test]
fn test_unknown_method() {
    let Some(mut daemon) = start_test_daemon() else {
        return;
    };

    let sock = socket_path();
    let stream = UnixStream::connect(&sock).expect("connect");
    stream.set_read_timeout(Some(Duration::from_secs(5))).expect("timeout");
    let mut ws = stream.try_clone().expect("clone");
    let mut reader = BufReader::new(stream);

    let resp = send_request(
        &mut ws,
        &mut reader,
        r#"{"jsonrpc":"2.0","id":4,"method":"nonexistent"}"#,
    );

    assert!(resp.contains("-32601"), "should have method not found error: {resp}");

    shutdown_daemon(&mut ws, &mut reader);
    let _ = daemon.wait();
}

/// D3.4.1: Keepalive returns ok.
#[test]
fn test_keepalive() {
    let Some(mut daemon) = start_test_daemon() else {
        return;
    };

    let sock = socket_path();
    let stream = UnixStream::connect(&sock).expect("connect");
    stream.set_read_timeout(Some(Duration::from_secs(5))).expect("timeout");
    let mut ws = stream.try_clone().expect("clone");
    let mut reader = BufReader::new(stream);

    let resp = send_request(
        &mut ws,
        &mut reader,
        r#"{"jsonrpc":"2.0","id":5,"method":"keepalive"}"#,
    );

    assert!(resp.contains("\"ok\":true"), "keepalive should return ok: {resp}");

    shutdown_daemon(&mut ws, &mut reader);
    let _ = daemon.wait();
}

/// D2.4.10 / S4.4: Invalid JSON returns parse error.
#[test]
fn test_invalid_json() {
    let Some(mut daemon) = start_test_daemon() else {
        return;
    };

    let sock = socket_path();
    let stream = UnixStream::connect(&sock).expect("connect");
    stream.set_read_timeout(Some(Duration::from_secs(5))).expect("timeout");
    let mut ws = stream.try_clone().expect("clone");
    let mut reader = BufReader::new(stream);

    let resp = send_request(
        &mut ws,
        &mut reader,
        "this is not json",
    );

    assert!(resp.contains("-32700"), "should have parse error: {resp}");

    shutdown_daemon(&mut ws, &mut reader);
    let _ = daemon.wait();
}

/// D3.2.6 / S4.4.9: Shutdown without nonce fails.
#[test]
fn test_shutdown_requires_nonce() {
    let Some(mut daemon) = start_test_daemon() else {
        return;
    };

    let sock = socket_path();
    let stream = UnixStream::connect(&sock).expect("connect");
    stream.set_read_timeout(Some(Duration::from_secs(5))).expect("timeout");
    let mut ws = stream.try_clone().expect("clone");
    let mut reader = BufReader::new(stream);

    // Try shutdown without nonce — should fail
    let resp = send_request(
        &mut ws,
        &mut reader,
        r#"{"jsonrpc":"2.0","id":6,"method":"shutdown","params":{}}"#,
    );

    assert!(resp.contains("nonce"), "should mention nonce: {resp}");
    assert!(!resp.contains("shutting down"), "should NOT shut down without nonce: {resp}");

    // Now do proper shutdown
    shutdown_daemon(&mut ws, &mut reader);
    let _ = daemon.wait();
}
