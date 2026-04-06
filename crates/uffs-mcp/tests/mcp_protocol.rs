//! MCP protocol end-to-end tests (D4.3).
//!
//! Spawns the `uffs-mcp` binary, pipes JSON-RPC to stdin, reads stdout,
//! and verifies the MCP protocol flow.
//!
//! NOTE: These tests require a running daemon OR will test the MCP
//! protocol error handling when the daemon is unavailable.
#![expect(
    clippy::tests_outside_test_module,
    reason = "integration tests are inherently outside cfg(test)"
)]
#![expect(
    clippy::print_stderr,
    reason = "eprintln is appropriate in integration tests for skip notices"
)]

// These crates are used by the uffs-mcp binary but not by this test target.
// Acknowledge them so `unused-crate-dependencies` doesn't fire.
use core::time::Duration;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, Command, Stdio};

use anyhow as _;
use serde as _;
use tokio as _;
use tracing as _;
use tracing_subscriber as _;
use uffs_client as _;

/// Spawn uffs-mcp with piped stdin/stdout.
fn spawn_mcp() -> Option<(
    Child,
    std::process::ChildStdin,
    BufReader<std::process::ChildStdout>,
)> {
    let mut exe = std::env::current_exe().expect("current_exe");
    exe.pop(); // remove test binary name
    exe.pop(); // remove deps/
    exe.push("uffs-mcp");
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
fn send_and_read(
    stdin: &mut std::process::ChildStdin,
    stdout: &mut BufReader<std::process::ChildStdout>,
    req: &str,
) -> Option<String> {
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

    if let Some(body) = &resp {
        assert!(
            body.contains("\"protocolVersion\""),
            "should have protocolVersion: {body}"
        );
        assert!(body.contains("\"tools\""), "should advertise tools: {body}");
        assert!(
            body.contains("\"resources\""),
            "should advertise resources: {body}"
        );
        assert!(
            body.contains("\"prompts\""),
            "should advertise prompts: {body}"
        );
        assert!(
            body.contains("\"uffs\""),
            "server name should be uffs: {body}"
        );
    }

    // Send initialized notification
    drop(send_and_read(
        &mut stdin,
        &mut stdout,
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
    ));

    drop(child.kill());
}

/// D4.3.1: Test tools/list returns our 4 tools.
#[test]
fn test_mcp_tools_list() {
    let Some((mut child, mut stdin, mut stdout)) = spawn_mcp() else {
        return;
    };

    // Initialize first
    drop(send_and_read(
        &mut stdin,
        &mut stdout,
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{}}}"#,
    ));

    let resp = send_and_read(
        &mut stdin,
        &mut stdout,
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}"#,
    );

    if let Some(body) = &resp {
        assert!(
            body.contains("uffs_search"),
            "should have uffs_search: {body}"
        );
        assert!(
            body.contains("uffs_drives"),
            "should have uffs_drives: {body}"
        );
        assert!(
            body.contains("uffs_status"),
            "should have uffs_status: {body}"
        );
        assert!(body.contains("uffs_info"), "should have uffs_info: {body}");
    }

    drop(child.kill());
}

/// D4.3.1: Test resources/list returns our 2 resources.
#[test]
fn test_mcp_resources_list() {
    let Some((mut child, mut stdin, mut stdout)) = spawn_mcp() else {
        return;
    };

    drop(send_and_read(
        &mut stdin,
        &mut stdout,
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{}}}"#,
    ));

    let resp = send_and_read(
        &mut stdin,
        &mut stdout,
        r#"{"jsonrpc":"2.0","id":3,"method":"resources/list","params":{}}"#,
    );

    if let Some(body) = &resp {
        assert!(
            body.contains("uffs://drives"),
            "should have drives resource: {body}"
        );
        assert!(
            body.contains("uffs://status"),
            "should have status resource: {body}"
        );
    }

    drop(child.kill());
}

/// D4.3.1: Test prompts/list returns our 4 prompts.
#[test]
fn test_mcp_prompts_list() {
    let Some((mut child, mut stdin, mut stdout)) = spawn_mcp() else {
        return;
    };

    drop(send_and_read(
        &mut stdin,
        &mut stdout,
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{}}}"#,
    ));

    let resp = send_and_read(
        &mut stdin,
        &mut stdout,
        r#"{"jsonrpc":"2.0","id":4,"method":"prompts/list","params":{}}"#,
    );

    if let Some(body) = &resp {
        assert!(
            body.contains("find_large_files"),
            "should have find_large_files: {body}"
        );
        assert!(
            body.contains("recent_changes"),
            "should have recent_changes: {body}"
        );
        assert!(
            body.contains("find_by_extension"),
            "should have find_by_extension: {body}"
        );
        assert!(
            body.contains("find_duplicates_by_name"),
            "should have find_duplicates: {body}"
        );
    }

    drop(child.kill());
}

/// D4.3.2: Claude Desktop MCP configuration example.
///
/// Add this to `~/Library/Application
/// Support/Claude/claude_desktop_config.json`:
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
    let command = parsed
        .get("mcpServers")
        .and_then(|servers| servers.get("uffs"))
        .and_then(|uffs| uffs.get("command"));
    assert!(command.is_some_and(serde_json::Value::is_string));
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
    let command = parsed.get("uffs").and_then(|uffs| uffs.get("command"));
    assert!(command.is_some_and(serde_json::Value::is_string));
}
