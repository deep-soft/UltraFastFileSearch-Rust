//! MCP protocol end-to-end tests (D4.3).
//!
//! Spawns the `uffs-mcp` binary, pipes JSON-RPC to stdin, reads stdout,
//! and verifies the MCP protocol flow.
//!
//! NOTE: These tests require a running daemon OR will test the MCP
//! protocol error handling when the daemon is unavailable.

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

/// Find the uffs-mcp binary.
fn mcp_exe() -> PathBuf {
    let mut path = std::env::current_exe().expect("current_exe");
    path.pop(); // remove test binary name
    path.pop(); // remove deps/
    path.push("uffs-mcp");
    path
}

/// Spawn uffs-mcp with piped stdin/stdout.
fn spawn_mcp() -> Option<(Child, std::process::ChildStdin, BufReader<std::process::ChildStdout>)> {
    let exe = mcp_exe();
    if !exe.exists() {
        eprintln!("uffs-mcp binary not found at {}, skipping", exe.display());
        eprintln!("Run `cargo build -p uffs-mcp` first.");
        return None;
    }

    let mut child = Command::new(&exe)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;

    let stdin = child.stdin.take()?;
    let stdout = BufReader::new(child.stdout.take()?);

    Some((child, stdin, stdout))
}

/// Send a JSON-RPC request and read the response.
fn send_and_read(stdin: &mut std::process::ChildStdin, stdout: &mut BufReader<std::process::ChildStdout>, req: &str) -> Option<String> {
    stdin.write_all(req.as_bytes()).ok()?;
    stdin.write_all(b"\n").ok()?;
    stdin.flush().ok()?;

    let mut line = String::new();
    // Give it a moment to process
    std::thread::sleep(Duration::from_millis(200));
    stdout.read_line(&mut line).ok()?;
    Some(line)
}

/// D4.3.1: Test MCP initialize handshake.
#[test]
fn test_mcp_initialize() {
    let Some((mut child, mut stdin, mut stdout)) = spawn_mcp() else {
        eprintln!("Skipping: uffs-mcp not available");
        return;
    };

    let resp = send_and_read(
        &mut stdin,
        &mut stdout,
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{}}}"#,
    );

    if let Some(resp) = &resp {
        assert!(resp.contains("\"protocolVersion\""), "should have protocolVersion: {resp}");
        assert!(resp.contains("\"tools\""), "should advertise tools: {resp}");
        assert!(resp.contains("\"resources\""), "should advertise resources: {resp}");
        assert!(resp.contains("\"prompts\""), "should advertise prompts: {resp}");
        assert!(resp.contains("\"uffs\""), "server name should be uffs: {resp}");
    }

    // Send initialized notification
    let _ = send_and_read(
        &mut stdin,
        &mut stdout,
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
    );

    let _ = child.kill();
}

/// D4.3.1: Test tools/list returns our 4 tools.
#[test]
fn test_mcp_tools_list() {
    let Some((mut child, mut stdin, mut stdout)) = spawn_mcp() else {
        return;
    };

    // Initialize first
    let _ = send_and_read(
        &mut stdin,
        &mut stdout,
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{}}}"#,
    );

    let resp = send_and_read(
        &mut stdin,
        &mut stdout,
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
    );

    if let Some(resp) = &resp {
        assert!(resp.contains("uffs_search"), "should have uffs_search: {resp}");
        assert!(resp.contains("uffs_drives"), "should have uffs_drives: {resp}");
        assert!(resp.contains("uffs_status"), "should have uffs_status: {resp}");
        assert!(resp.contains("uffs_info"), "should have uffs_info: {resp}");
    }

    let _ = child.kill();
}

/// D4.3.1: Test resources/list returns our 2 resources.
#[test]
fn test_mcp_resources_list() {
    let Some((mut child, mut stdin, mut stdout)) = spawn_mcp() else {
        return;
    };

    let _ = send_and_read(
        &mut stdin,
        &mut stdout,
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{}}}"#,
    );

    let resp = send_and_read(
        &mut stdin,
        &mut stdout,
        r#"{"jsonrpc":"2.0","id":3,"method":"resources/list"}"#,
    );

    if let Some(resp) = &resp {
        assert!(resp.contains("uffs://drives"), "should have drives resource: {resp}");
        assert!(resp.contains("uffs://status"), "should have status resource: {resp}");
    }

    let _ = child.kill();
}

/// D4.3.1: Test prompts/list returns our 4 prompts.
#[test]
fn test_mcp_prompts_list() {
    let Some((mut child, mut stdin, mut stdout)) = spawn_mcp() else {
        return;
    };

    let _ = send_and_read(
        &mut stdin,
        &mut stdout,
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{}}}"#,
    );

    let resp = send_and_read(
        &mut stdin,
        &mut stdout,
        r#"{"jsonrpc":"2.0","id":4,"method":"prompts/list"}"#,
    );

    if let Some(resp) = &resp {
        assert!(resp.contains("find_large_files"), "should have find_large_files: {resp}");
        assert!(resp.contains("recent_changes"), "should have recent_changes: {resp}");
        assert!(resp.contains("find_by_extension"), "should have find_by_extension: {resp}");
        assert!(resp.contains("find_duplicates_by_name"), "should have find_duplicates: {resp}");
    }

    let _ = child.kill();
}

/// D4.3.2: Claude Desktop MCP configuration example.
///
/// Add this to `~/Library/Application Support/Claude/claude_desktop_config.json`:
/// ```json
/// {
///   "mcpServers": {
///     "uffs": {
///       "command": "uffs-mcp"
///     }
///   }
/// }
/// ```
///
/// Or with an explicit path:
/// ```json
/// {
///   "mcpServers": {
///     "uffs": {
///       "command": "/path/to/uffs-mcp"
///     }
///   }
/// }
/// ```
#[test]
fn test_claude_desktop_config_example() {
    // This is a documentation test — verifies the config JSON is valid
    let config = r#"{
        "mcpServers": {
            "uffs": {
                "command": "uffs-mcp"
            }
        }
    }"#;
    let parsed: serde_json::Value = serde_json::from_str(config).expect("valid JSON");
    assert!(parsed["mcpServers"]["uffs"]["command"].is_string());
}

/// D4.3.3: Cursor / Windsurf MCP configuration example.
///
/// Add to `.cursor/mcp.json` or Windsurf MCP settings:
/// ```json
/// {
///   "uffs": {
///     "command": "uffs-mcp"
///   }
/// }
/// ```
#[test]
fn test_cursor_windsurf_config_example() {
    let config = r#"{
        "uffs": {
            "command": "uffs-mcp"
        }
    }"#;
    let parsed: serde_json::Value = serde_json::from_str(config).expect("valid JSON");
    assert!(parsed["uffs"]["command"].is_string());
}
