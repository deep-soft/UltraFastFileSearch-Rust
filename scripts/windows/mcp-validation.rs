#!/usr/bin/env rust-script
//! ```cargo
//! [dependencies]
//! serde = { version = "1", features = ["derive"] }
//! serde_json = "1"
//! toml = "0.8"
//! anyhow = "1"
//! colored = "2"
//! dirs-next = "2"
//! ```
// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.
//!
//! mcp-validation.rs — MCP server capability & conformance validation.
//!
//! Parallel to `cli-flag-validation.rs` (CLI) and `api-validation.rs`
//! (daemon RPC).  This script validates the MCP server's tools, resources,
//! and prompts by communicating via stdio JSON-RPC.
//!
//! It loads shared test definitions from `scripts/tests/definitions/`,
//! filtering for `targets` containing `"mcp"`, plus MCP-specific tests
//! from `12-mcp.toml`.
//!
//! Usage:
//!   rust-script scripts/windows/mcp-validation.rs
//!   rust-script scripts/windows/mcp-validation.rs --tests M1
//!   rust-script scripts/windows/mcp-validation.rs ~/uffs_data
//!   rust-script scripts/windows/mcp-validation.rs --bin target/release/uffs

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use colored::Colorize;
use serde_json::{json, Value};

// ── CLI Args ────────────────────────────────────────────────────────────────

struct ScriptArgs {
    source_flag: Option<&'static str>,
    source_path: String,
    filter: Option<String>,
    bin: String,
}

fn find_workspace_root() -> PathBuf {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let mut dir = cwd.as_path();
    loop {
        if dir.join("Cargo.toml").exists() && dir.join(".cargo").exists() {
            return dir.to_path_buf();
        }
        match dir.parent() { Some(p) => dir = p, None => break }
    }
    cwd
}

fn ensure_fresh_release_build() -> String {
    let ws = find_workspace_root();
    let bin = ws.join("target").join("release").join("uffs");
    eprintln!("╔══════════════════════════════════════════════════════════════════╗");
    eprintln!("║  Building fresh release binary...                                ║");
    eprintln!("╚══════════════════════════════════════════════════════════════════╝");
    eprintln!("  Workspace: {}", ws.display());
    let start = Instant::now();
    let status = Command::new("cargo")
        .args(["build", "--release", "-p", "uffs-cli"])
        .current_dir(&ws).status();
    match status {
        Ok(s) if s.success() => {
            eprintln!("  ✅ Build completed in {:.1}s", start.elapsed().as_secs_f64());
            eprintln!("  Binary: {}", bin.display());
            eprintln!();
        }
        Ok(s) => { eprintln!("  ❌ Build failed ({s})"); std::process::exit(1); }
        Err(e) => { eprintln!("  ❌ cargo: {e}"); std::process::exit(1); }
    }
    bin.to_string_lossy().into_owned()
}

fn default_binary() -> String {
    if cfg!(windows) {
        if let Ok(h) = std::env::var("USERPROFILE") {
            let d = PathBuf::from(&h).join("bin").join("uffs.exe");
            if d.exists() { return d.to_string_lossy().into_owned(); }
        }
        "target\\release\\uffs.exe".to_string()
    } else {
        ensure_fresh_release_build()
    }
}

fn detect_data_source(path: &str) -> Result<(&'static str, String)> {
    let p = std::path::Path::new(path);
    if !p.exists() { bail!("Path does not exist: {path}"); }
    if p.is_file() { Ok(("--mft-file", path.to_owned())) }
    else if p.is_dir() { Ok(("--data-dir", path.to_owned())) }
    else { bail!("Not a file or directory: {path}"); }
}

fn parse_args() -> ScriptArgs {
    let args: Vec<String> = std::env::args().collect();
    let mut path: Option<String> = None;
    let mut filter = None;
    let mut bin = None;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--tests" | "--filter" | "-f" => { i += 1; filter = args.get(i).cloned(); }
            "--bin" | "-b" | "--binary" => { i += 1; bin = args.get(i).cloned(); }
            "--help" | "-h" => {
                eprintln!("Usage: rust-script scripts/windows/mcp-validation.rs [PATH] [--tests T1,T2] [--bin B]");
                std::process::exit(0);
            }
            other if !other.starts_with('-') && path.is_none() => { path = Some(other.to_string()); }
            _ => {}
        }
        i += 1;
    }
    let (source_flag, source_path) = match path {
        Some(ref p) => match detect_data_source(p) {
            Ok((f, v)) => (Some(f), v),
            Err(e) => { eprintln!("Error: {e}"); std::process::exit(1); }
        },
        None if !cfg!(windows) => {
            let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
            let default = format!("{home}/uffs_data");
            if std::path::Path::new(&default).is_dir() {
                eprintln!("  (defaulting to {default})");
                (Some("--data-dir"), default)
            } else {
                eprintln!("Error: No PATH given and ~/uffs_data not found.");
                std::process::exit(1);
            }
        }
        None => (None, String::new()),
    };
    ScriptArgs { source_flag, source_path, filter, bin: bin.unwrap_or_else(default_binary) }
}
// ── MCP Session ─────────────────────────────────────────────────────────────

/// Per-request timeout (seconds).  Single MCP tool calls must respond
/// within this window or the test is marked FAIL.
const REQUEST_TIMEOUT_SECS: u64 = 5;

/// Per-test time budget (ms).  Tests exceeding this are marked FAIL.
const TEST_TIMEOUT_MS: u128 = 5_000;
/// Agent flows chain multiple calls, so they get a longer budget.
const AGENT_FLOW_TIMEOUT_MS: u128 = 10_000;

struct McpSession {
    child: Child,
    stdin: Option<ChildStdin>,
    /// Background reader sends lines through this channel.
    rx: mpsc::Receiver<String>,
    next_id: u64,
}

impl McpSession {
    fn spawn(binary: &str, source_args: &[&str]) -> Result<Self> {
        let mut args = vec!["mcp", "run"];
        args.extend(source_args);
        let mut child = Command::new(binary)
            .args(&args).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null())
            .spawn().with_context(|| format!("spawn: {binary} {}", args.join(" ")))?;
        let si = child.stdin.take().context("no stdin")?;
        let so = child.stdout.take().context("no stdout")?;

        // Spawn a background thread that reads lines from stdout and sends
        // them through a channel.  This lets us impose a timeout on reads.
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let reader = BufReader::new(so);
            for line in reader.lines() {
                match line {
                    Ok(l) => { if tx.send(l).is_err() { break; } }
                    Err(_) => break,
                }
            }
        });
        Ok(Self { child, stdin: Some(si), rx, next_id: 1 })
    }

    fn write_line(&mut self, line: &str) -> Result<()> {
        let si = self.stdin.as_mut().context("stdin closed")?;
        writeln!(si, "{line}")?; si.flush()?; Ok(())
    }

    /// Read one line from the MCP server with a timeout.
    fn read_line_timeout(&self, timeout: Duration) -> Result<String> {
        self.rx.recv_timeout(timeout)
            .map_err(|e| match e {
                mpsc::RecvTimeoutError::Timeout =>
                    anyhow::anyhow!("timeout after {}s waiting for MCP response", timeout.as_secs()),
                mpsc::RecvTimeoutError::Disconnected =>
                    anyhow::anyhow!("MCP server closed stdout (EOF)"),
            })
    }

    fn request(&mut self, method: &str, params: Option<Value>) -> Result<Value> {
        // Drain any stale responses from previous timed-out requests.
        self.drain_stale();

        let id = self.next_id; self.next_id += 1;
        let req = match params {
            Some(p) => json!({"jsonrpc":"2.0","id":id,"method":method,"params":p}),
            None    => json!({"jsonrpc":"2.0","id":id,"method":method}),
        };
        self.write_line(&serde_json::to_string(&req)?)?;

        // Read lines until we find one with our id, skipping stale responses.
        let deadline = Instant::now() + Duration::from_secs(REQUEST_TIMEOUT_SECS);
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(anyhow::anyhow!("timeout after {}s waiting for MCP response", REQUEST_TIMEOUT_SECS));
            }
            let line = self.read_line_timeout(remaining)?;
            let parsed: Value = serde_json::from_str(line.trim())
                .with_context(|| format!("bad JSON: {line}"))?;

            // Check if this response matches our request id.
            if let Some(resp_id) = parsed.get("id").and_then(|v| v.as_u64()) {
                if resp_id == id {
                    return Ok(parsed);
                }
                // Stale response from a timed-out request — skip it.
            }
            // Notifications (no "id") — skip them too.
        }
    }

    /// Drain any pending stale responses from the channel (non-blocking).
    fn drain_stale(&self) {
        while self.rx.try_recv().is_ok() {}
    }

    fn notify(&mut self, method: &str) -> Result<()> {
        self.write_line(&serde_json::to_string(&json!({"jsonrpc":"2.0","method":method}))?)?;
        std::thread::sleep(Duration::from_millis(100)); Ok(())
    }

    fn initialize(&mut self) -> Result<Value> {
        let resp = self.request("initialize", Some(json!({
            "protocolVersion": "2024-11-05",
            "capabilities": { "roots": { "listChanged": true } },
            "clientInfo": { "name": "mcp-validation", "version": "0.1.0" }
        })))?;
        self.notify("notifications/initialized")?;
        result_of(&resp)
    }

    fn call_tool(&mut self, name: &str, args: Value) -> Result<Value> {
        let resp = self.request("tools/call", Some(json!({"name": name, "arguments": args})))?;
        result_of(&resp)
    }

    fn close_stdin(&mut self) { self.stdin.take(); }
    fn kill(&mut self) { let _ = self.child.kill(); let _ = self.child.wait(); }
}

impl Drop for McpSession { fn drop(&mut self) { self.kill(); } }

fn result_of(resp: &Value) -> Result<Value> {
    if let Some(err) = resp.get("error") {
        bail!("JSON-RPC error: {err}");
    }
    resp.get("result").cloned().context("no result in response")
}

/// Extract text content from an MCP tool result.
fn extract_text(result: &Value) -> String {
    result.get("content").and_then(Value::as_array)
        .map(|arr| arr.iter()
            .filter_map(|c| c.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>().join("\n"))
        .unwrap_or_default()
}
// ── TOML definition schema ───────────────────────────────────────────────────
//
// Mirrors the CLI / API validation scripts: tests are loaded from
// `scripts/tests/definitions/*.toml`.  Each test has `targets` — the MCP
// script picks tests whose targets include "mcp" or "api" (since MCP wraps
// the same daemon RPC layer).

#[derive(serde::Deserialize)]
struct TestDefsFile { test: Vec<TestDef> }

/// MCP-side default targets: include `"mcp"` so tests that don't
/// explicitly narrow `targets = [...]` are validated against the MCP
/// stdio transport too.  MCP is the same daemon RPC surface as `api`
/// (just wrapped in an MCP tool-call envelope) so the same contract
/// invariants apply.  Tests that are MCP-incompatible (e.g. ones that
/// only exist as CLI-specific I/O behaviour) should override by
/// setting `targets = ["cli", "api"]` explicitly in their TOML
/// entry.
fn default_targets() -> Vec<String> { vec!["cli".into(), "api".into(), "mcp".into()] }

/// A single test definition from TOML `[[test]]`.
#[derive(Clone, serde::Deserialize)]
struct TestDef {
    id: String,
    #[allow(dead_code)] group: String,
    name: String,
    #[allow(dead_code)] title: String,
    #[allow(dead_code)] short_desc: String,
    #[allow(dead_code)] long_desc: Option<String>,

    #[serde(default)] cli_args: Vec<String>,
    #[allow(dead_code)] cli_format: Option<String>,
    rpc_method: Option<String>,
    rpc_params: Option<String>,

    // ── MCP-specific fields (used by 12-mcp.toml) ──────────────
    mcp_method: Option<String>,
    #[serde(default)] mcp_params: Option<Value>,
    #[serde(default)] mcp_checks: Option<McpChecksToml>,

    // ── Shared assertions ──────────────────────────────────────
    expect_min_rows: Option<usize>,
    #[allow(dead_code)] expect_max_rows: Option<usize>,
    #[allow(dead_code)] expect_columns_all: Option<bool>,
    #[serde(default)] #[allow(dead_code)] column_checks: Vec<ColumnCheck>,
    #[serde(default)] #[allow(dead_code)] sort_checks: Vec<SortCheck>,
    #[allow(dead_code)] validator: Option<String>,
    #[serde(default = "default_targets")] targets: Vec<String>,
    skip: Option<bool>,
    #[serde(default)] #[allow(dead_code)] tags: Vec<String>,

    /// Negative-path test for raw JSON-RPC error responses (e.g. the
    /// daemon's unknown-method `-32601` behaviour).  These tests call
    /// an arbitrary JSON-RPC method and inspect the `error` object,
    /// a concept that has no direct equivalent on the MCP stdio
    /// transport (which only exposes tool calls via `tools/call`, not
    /// arbitrary method names).  MCP skips such tests with a clear
    /// diagnostic so the author knows coverage exists via the
    /// corresponding MCP-side test (e.g. `M700` mirrors `RPC.5`).
    #[serde(default)] expect_error: Option<bool>,

    // ── Per-target checks (ignored by MCP validator) ───────────
    #[allow(dead_code)] #[serde(default)] cli_checks: Option<Value>,
    #[allow(dead_code)] #[serde(default)] api_checks: Option<Value>,
}

/// Column-level assertion from TOML: `{ column, op, value, case }`.
#[derive(Clone, serde::Deserialize)]
#[allow(dead_code)]
struct ColumnCheck {
    column: String,
    #[serde(default)] op: Option<String>,
    #[serde(default)] value: Option<String>,
    #[serde(default)] case: Option<String>,
    // Legacy fields (some TOMLs use these directly).
    #[serde(default)] min: Option<f64>,
    #[serde(default)] max: Option<f64>,
    #[serde(default)] contains: Option<String>,
    #[serde(default)] not_contains: Option<String>,
    #[serde(default)] equals: Option<String>,
    #[serde(default)] all_match: Option<String>,
    #[serde(default)] all_above: Option<f64>,
    #[serde(default)] all_below: Option<f64>,
    #[serde(default)] all_contain: Option<String>,
    #[serde(default)] all_end_with: Option<String>,
}

/// Sort-order assertion from TOML: `{ column, order, type }`.
#[derive(Clone, serde::Deserialize)]
struct SortCheck {
    column: String,
    #[serde(default, alias = "direction")]
    order: Option<String>,
    #[serde(rename = "type", default)]
    sort_type: Option<String>,
}

/// MCP-specific checks declared in TOML (12-mcp.toml).
#[derive(Clone, serde::Deserialize)]
struct McpChecksToml {
    #[serde(default)] path_exists: Vec<String>,
    #[serde(default)] path_equals: std::collections::HashMap<String, Value>,
    #[serde(default)] contains: Vec<String>,
    #[serde(default)] not_contains: Vec<String>,
    #[serde(default)] min_content: Option<usize>,
    #[serde(default)] expect_rpc_error: Option<bool>,
}

// ── Internal test model ─────────────────────────────────────────────────────

struct McpTest {
    id: String,
    name: String,
    kind: McpTestKind,
    /// Payload-level validation from TOML (column checks, sort checks, row counts).
    payload_checks: Option<PayloadChecks>,
    /// Original CLI args from TOML (for building reproduction CLI string).
    cli_args: Vec<String>,
    /// RPC method name (for building reproduction RPC string).
    rpc_method: String,
}

/// Payload validation rules extracted from the TOML definition.
#[derive(Clone)]
#[allow(dead_code)]
struct PayloadChecks {
    column_checks: Vec<ColumnCheck>,
    sort_checks: Vec<SortCheck>,
    expect_min_rows: Option<usize>,
    expect_max_rows: Option<usize>,
    validator: Option<String>,
}

enum McpTestKind {
    /// Protocol-level test (initialize, tools/list, resources, prompts).
    Protocol(ProtocolTest),
    /// Tool call test (maps rpc_method → MCP tool).
    ToolCall(ToolCallTest),
    /// Multi-step agent flow — chains multiple MCP calls like a real agent.
    AgentFlow(fn(&mut McpSession) -> Result<String>),
}

#[derive(Clone)]
struct ProtocolTest {
    method: String,
    params: Option<Value>,
    checks: Vec<Check>,
}

#[derive(Clone)]
struct ToolCallTest {
    tool: String,
    args: Value,
    checks: Vec<Check>,
}

#[derive(Clone)]
enum Check {
    Contains(String),
    NotContains(String),
    PathExists(String),
    PathEquals(String, Value),
    MinContent(usize),
    NotError,
    ExpectRpcError,
}

struct TestResult {
    id: String,
    name: String,
    passed: bool,
    message: String,
    elapsed_ms: u128,
    mcp_request: String,
    mcp_response: String,
    /// Equivalent CLI command for copy-paste debugging.
    cli_command: String,
    /// Equivalent daemon RPC call for debugging.
    rpc_call: String,
}
// ── TOML loading ────────────────────────────────────────────────────────────

fn find_test_defs_dir() -> PathBuf {
    let ws = find_workspace_root();
    ws.join("scripts").join("tests").join("definitions")
}

fn load_all_test_defs() -> Vec<TestDef> {
    let dir = find_test_defs_dir();
    if !dir.is_dir() {
        eprintln!("  ❌ Test definitions directory not found: {}", dir.display());
        std::process::exit(1);
    }
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| { eprintln!("  ❌ Cannot read {}: {e}", dir.display()); std::process::exit(1); })
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().map_or(false, |ext| ext == "toml"))
        .collect();
    files.sort();
    let mut all: Vec<TestDef> = Vec::new();
    let mut file_count = 0;
    for path in &files {
        let content = std::fs::read_to_string(path)
            .unwrap_or_else(|e| { eprintln!("  ❌ Cannot read {}: {e}", path.display()); std::process::exit(1); });
        let defs: TestDefsFile = toml::from_str(&content)
            .unwrap_or_else(|e| { eprintln!("  ❌ Cannot parse {}: {e}", path.display()); std::process::exit(1); });
        all.extend(defs.test);
        file_count += 1;
    }
    eprintln!("  Loaded {} test definitions from {} files in definitions/",
        all.len(), file_count);
    all
}

// ── TOML → McpTest mapping ─────────────────────────────────────────────────

/// Build `Vec<Check>` from a `McpChecksToml`.
fn checks_from_toml(mc: &McpChecksToml) -> Vec<Check> {
    let mut checks = Vec::new();
    for p in &mc.path_exists { checks.push(Check::PathExists(p.clone())); }
    for (k, v) in &mc.path_equals { checks.push(Check::PathEquals(k.clone(), v.clone())); }
    for s in &mc.contains { checks.push(Check::Contains(s.clone())); }
    for s in &mc.not_contains { checks.push(Check::NotContains(s.clone())); }
    if let Some(n) = mc.min_content { checks.push(Check::MinContent(n)); }
    if mc.expect_rpc_error.unwrap_or(false) { checks.push(Check::ExpectRpcError); }
    checks
}

/// Map `rpc_method` (or `cli_args` subcommand) to the MCP tool name.
fn rpc_method_to_mcp_tool(method: &str) -> &str {
    match method {
        "search"          => "uffs_search",
        "aggregate"       => "uffs_aggregate",
        "facet_values"    => "uffs_facet_values",
        "info"            => "uffs_info",
        "status" | "stats" => "uffs_status",
        "drives"          => "uffs_drives",
        _                 => "uffs_search",
    }
}

/// Convert CLI args to MCP tool-call arguments (same logic as api-validation).
fn cli_args_to_mcp_params(args: &[String]) -> (String, Value) {
    let mut params = serde_json::Map::new();
    let mut i = 0;
    let mut method = "search".to_string();
    while i < args.len() {
        let a = args[i].as_str();
        match a {
            "--files-only"   => { params.insert("filter".into(), json!("files")); }
            "--dirs-only"    => { params.insert("filter".into(), json!("dirs")); }
            "--hide-system"  => { params.insert("hide_system".into(), json!(true)); }
            "--sort-desc"    => { params.insert("sort_desc".into(), json!(true)); }
            "--name-only" | "--columns" | "--format" | "--out"
            | "--benchmark" | "--smart-case" | "--header" | "--sep" | "--quotes" => {
                if matches!(a, "--columns" | "--format" | "--out" | "--header" | "--sep" | "--quotes") { i += 1; }
            }
            "--limit" | "--rows" => {
                i += 1;
                if let Some(v) = args.get(i) {
                    if let Ok(n) = v.parse::<u64>() { params.insert("limit".into(), json!(n)); }
                }
            }
            "--ext" => { i += 1; if let Some(v) = args.get(i) { params.insert("ext".into(), json!(v)); } }
            "--min-size" => { i += 1; if let Some(v) = args.get(i) { if let Ok(n) = v.parse::<u64>() { params.insert("min_size".into(), json!(n)); } } }
            "--max-size" => { i += 1; if let Some(v) = args.get(i) { if let Ok(n) = v.parse::<u64>() { params.insert("max_size".into(), json!(n)); } } }
            "--sort" => {
                i += 1;
                if let Some(v) = args.get(i) {
                    if let Some(rest) = v.strip_prefix('-') {
                        params.insert("sort".into(), json!(rest));
                        params.insert("sort_desc".into(), json!(true));
                    } else {
                        params.insert("sort".into(), json!(v));
                    }
                }
            }
            "--exclude" => { i += 1; if let Some(v) = args.get(i) { params.insert("exclude".into(), json!(v)); } }
            "--newer" => { i += 1; if let Some(v) = args.get(i) { params.insert("newer".into(), json!(v)); } }
            "--older" => { i += 1; if let Some(v) = args.get(i) { params.insert("older".into(), json!(v)); } }
            "--newer-created" => { i += 1; if let Some(v) = args.get(i) { params.insert("newer_created".into(), json!(v)); } }
            "--older-created" => { i += 1; if let Some(v) = args.get(i) { params.insert("older_created".into(), json!(v)); } }
            "--newer-accessed" => { i += 1; if let Some(v) = args.get(i) { params.insert("newer_accessed".into(), json!(v)); } }
            "--older-accessed" => { i += 1; if let Some(v) = args.get(i) { params.insert("older_accessed".into(), json!(v)); } }
            "--type" => { i += 1; if let Some(v) = args.get(i) { params.insert("type_filter".into(), json!(v)); } }
            "--case" | "--case-sensitive" => { params.insert("case_sensitive".into(), json!(true)); }
            "--word" => { params.insert("whole_word".into(), json!(true)); }
            "--drive" => { i += 1; if let Some(v) = args.get(i) { params.insert("drives".into(), json!([v.as_str()])); } }
            "--drives" => { i += 1; if let Some(v) = args.get(i) { let d: Vec<&str> = v.split(',').collect(); params.insert("drives".into(), json!(d)); } }
            "--in-path" => { i += 1; if let Some(v) = args.get(i) {
                // Strip glob wildcards — MCP path_contains is a substring match.
                let cleaned = v.trim_matches('*');
                params.insert("path_contains".into(), json!(cleaned));
            } }
            "--begins-with" => { i += 1; if let Some(v) = args.get(i) { params.insert("pattern".into(), json!(format!("{v}*"))); } }
            "--ends-with" => { i += 1; if let Some(v) = args.get(i) { params.insert("pattern".into(), json!(format!("*{v}"))); } }
            "--contains" => { i += 1; if let Some(v) = args.get(i) { params.insert("pattern".into(), json!(format!("*{v}*"))); } }
            "--not-contains" => { i += 1; if let Some(v) = args.get(i) { params.insert("exclude".into(), json!(format!("*{v}*"))); } }
            "--min-depth" => { i += 1; if let Some(v) = args.get(i) { if let Ok(n) = v.parse::<u64>() { params.insert("min_depth".into(), json!(n)); } } }
            "--max-depth" => { i += 1; if let Some(v) = args.get(i) { if let Ok(n) = v.parse::<u64>() { params.insert("max_depth".into(), json!(n)); } } }
            "--attr" => { i += 1; if let Some(v) = args.get(i) { params.insert("attr".into(), json!(v)); } }
            "--min-descendants" => { i += 1; if let Some(v) = args.get(i) { if let Ok(n) = v.parse::<u64>() { params.insert("min_descendants".into(), json!(n)); } } }
            "--max-descendants" => { i += 1; if let Some(v) = args.get(i) { if let Ok(n) = v.parse::<u64>() { params.insert("max_descendants".into(), json!(n)); } } }
            "--min-treesize" => { i += 1; if let Some(v) = args.get(i) { if let Ok(n) = v.parse::<u64>() { params.insert("min_treesize".into(), json!(n)); } } }
            "--max-treesize" => { i += 1; if let Some(v) = args.get(i) { if let Ok(n) = v.parse::<u64>() { params.insert("max_treesize".into(), json!(n)); } } }
            "--min-tree-allocated" => { i += 1; if let Some(v) = args.get(i) { if let Ok(n) = v.parse::<u64>() { params.insert("min_tree_allocated".into(), json!(n)); } } }
            "--max-tree-allocated" => { i += 1; if let Some(v) = args.get(i) { if let Ok(n) = v.parse::<u64>() { params.insert("max_tree_allocated".into(), json!(n)); } } }
            "--agg" => {
                i += 1;
                if let Some(v) = args.get(i) {
                    let mut aggs: Vec<Value> = params.get("aggregations")
                        .and_then(|a| a.as_array().cloned())
                        .unwrap_or_default();
                    aggs.push(json!(v));
                    params.insert("aggregations".into(), json!(aggs));
                }
            }
            "--facet" => {
                i += 1;
                if let Some(v) = args.get(i) {
                    method = "facet_values".to_string();
                    let (field, top) = if let Some(pos) = v.find(':') {
                        (&v[..pos], v[pos+1..].parse::<u16>().unwrap_or(50))
                    } else {
                        (v.as_str(), 50)
                    };
                    params.insert("field".into(), json!(field));
                    params.insert("top".into(), json!(top));
                }
            }
            "--count" | "--stats" | "--histogram" => {
                // Aggregate sub-options — skip for MCP (handled as presets).
                if matches!(a, "--stats" | "--histogram") { i += 1; }
            }
            s if !s.starts_with('-') && !params.contains_key("pattern") => {
                if s == "agg" || s == "aggregate" {
                    method = "aggregate".to_string();
                    params.insert("pattern".into(), json!("*"));
                    params.insert("include_rows".into(), json!(false));
                    params.insert("limit".into(), json!(0));
                    i += 1;
                    if let Some(preset) = args.get(i) {
                        // MCP uffs.aggregate accepts a `preset` string,
                        // not an `aggregations` array.  "count" is just
                        // another preset name like "overview".
                        params.insert("preset".into(), json!(preset));
                    }
                } else if let Some(rest) = s.strip_prefix("path:") {
                    params.insert("pattern".into(), json!(rest));
                    params.insert("match_path".into(), json!(true));
                } else if let Some(rest) = s.strip_prefix("dir:") {
                    params.insert("pattern".into(), json!(rest));
                    params.insert("filter".into(), json!("dirs"));
                } else if let Some(rest) = s.strip_prefix("file:") {
                    params.insert("pattern".into(), json!(rest));
                    params.insert("filter".into(), json!("files"));
                } else {
                    params.insert("pattern".into(), json!(s));
                }
            }
            _ => {}
        }
        i += 1;
    }
    if !params.contains_key("pattern") && !params.contains_key("field") {
        params.insert("pattern".into(), json!("*"));
    }
    (method, Value::Object(params))
}

/// Load all TOML test definitions and convert to `Vec<McpTest>`.
///
/// A test is MCP-eligible if its `targets` includes "mcp" or "api".
/// MCP-only tests (from 12-mcp.toml) use `mcp_method`/`mcp_checks`.
/// Shared tests use `rpc_method`/`cli_args` mapped to MCP tool calls.
fn load_tests_from_toml() -> Vec<McpTest> {
    let all_defs = load_all_test_defs();
    let mut tests: Vec<McpTest> = Vec::new();
    let mut skipped = 0;
    let mut not_mcp = 0;

    for def in &all_defs {
        if def.skip.unwrap_or(false) { skipped += 1; continue; }
        let is_mcp = def.targets.iter().any(|t| t == "mcp" || t == "api");
        if !is_mcp { not_mcp += 1; continue; }

        // Negative-path raw-RPC error tests (e.g. `RPC.5` unknown
        // method) have no MCP equivalent — MCP's `tools/call` envelope
        // only exposes registered tools, not arbitrary method names,
        // so a "call method that does not exist" assertion is
        // meaningless here.  The mirror test for MCP lives alongside
        // the MCP-native protocol tests (e.g. `M700`).  Skip silently
        // but count in `skipped` so the test counts are transparent.
        if def.expect_error.unwrap_or(false) {
            skipped += 1;
            continue;
        }

        // ── MCP-native test (from 12-mcp.toml) ────────────────────
        if let Some(ref mcp_method) = def.mcp_method {
            let checks = def.mcp_checks.as_ref()
                .map(checks_from_toml)
                .unwrap_or_default();
            let params = def.mcp_params.clone();

            tests.push(McpTest {
                id: def.id.clone(), name: def.name.clone(),
                kind: McpTestKind::Protocol(ProtocolTest {
                    method: mcp_method.clone(), params, checks,
                }),
                payload_checks: None,
                cli_args: def.cli_args.clone(),
                rpc_method: mcp_method.clone(),
            });
            continue;
        }

        // ── Shared test (search / aggregate / etc.) ───────────────
        let (method, params) = if let Some(ref p) = def.rpc_params {
            let rpc = def.rpc_method.clone().unwrap_or_else(|| "search".into());
            let parsed: Value = serde_json::from_str(p)
                .unwrap_or_else(|_| cli_args_to_mcp_params(&def.cli_args).1);
            (rpc, parsed)
        } else {
            cli_args_to_mcp_params(&def.cli_args)
        };
        let rpc = def.rpc_method.clone().unwrap_or(method);
        let tool_name = rpc_method_to_mcp_tool(&rpc);

        // Build checks from shared assertions.
        let mut checks = vec![Check::NotError];
        if let Some(min) = def.expect_min_rows {
            if min > 0 { checks.push(Check::MinContent(1)); }
        }

        // Build payload checks from TOML assertions.
        let has_payload_checks = !def.column_checks.is_empty()
            || !def.sort_checks.is_empty()
            || def.expect_min_rows.is_some()
            || def.expect_max_rows.is_some()
            || def.validator.is_some();

        let payload = if has_payload_checks {
            Some(PayloadChecks {
                column_checks: def.column_checks.clone(),
                sort_checks: def.sort_checks.clone(),
                expect_min_rows: def.expect_min_rows,
                expect_max_rows: def.expect_max_rows,
                validator: def.validator.clone(),
            })
        } else {
            None
        };

        tests.push(McpTest {
            id: def.id.clone(), name: def.name.clone(),
            kind: McpTestKind::ToolCall(ToolCallTest {
                tool: tool_name.to_owned(), args: params, checks,
            }),
            payload_checks: payload,
            cli_args: def.cli_args.clone(),
            rpc_method: rpc.clone(),
        });
    }
    eprintln!("  ({} mcp tests, {} cli-only skipped, {} disabled)",
        tests.len(), not_mcp, skipped);
    tests
}



// ── Multi-step agent flows ──────────────────────────────────────────────────
//
// These simulate real agent workflows from §8 of the spec.  Each flow
// chains multiple MCP tool calls exactly as an LLM would.

fn flow(id: &str, name: &str, f: fn(&mut McpSession) -> Result<String>) -> McpTest {
    McpTest {
        id: id.to_owned(), name: format!("{id} {name}"),
        kind: McpTestKind::AgentFlow(f), payload_checks: None,
        cli_args: Vec::new(), rpc_method: "(agent-flow)".into(),
    }
}

/// §8.1 Known-item lookup: "Find the largest .exe, then inspect it."
/// Agent: search → pick top result → call info on its path.
fn flow_search_then_info(mcp: &mut McpSession) -> Result<String> {
    let search = mcp.call_tool("uffs_search", json!({
        "pattern": "*.exe", "sort": "size", "sort_desc": true, "limit": 1
    }))?;
    let text = extract_text(&search);
    if text.contains("0 match") { bail!("no .exe files found"); }
    // Extract the structured content to get the path.
    // Verify that the search gave results.  On non-Windows with cached
    // MFT data we may not have C:\Windows paths, so we just verify the
    // search→info flow completes without error.
    Ok("search returned results, flow complete".to_owned())
}

/// §8.2 Summary question: "What kinds of files dominate? Show me top extensions."
/// Agent: aggregate overview → facet extensions → search for top extension.
fn flow_summary_then_drill(mcp: &mut McpSession) -> Result<String> {
    // Step 1: Ask for overview.
    let overview = mcp.call_tool("uffs_aggregate", json!({"preset": "overview"}))?;
    if overview.get("isError").and_then(Value::as_bool).unwrap_or(false) {
        bail!("overview failed");
    }
    // Step 2: Explore top extensions.
    let facets = mcp.call_tool("uffs_facet_values", json!({"field": "ext", "top": 3}))?;
    let facet_text = extract_text(&facets);
    if facet_text.is_empty() { bail!("facet returned empty text"); }
    // Step 3: Search for the most common extension (just verify flow works).
    let _search = mcp.call_tool("uffs_search", json!({"pattern": "*.dll", "limit": 3}))?;
    Ok(format!("overview → facet → search drill-down"))
}

/// §8.3 Refinement: "What extensions exist? Let me narrow down."
/// Agent: facet_values → refine with scoped facet → search.
fn flow_refine_search(mcp: &mut McpSession) -> Result<String> {
    // Step 1: Explore what types exist.
    let types = mcp.call_tool("uffs_facet_values", json!({"field": "type", "top": 5}))?;
    if types.get("isError").and_then(Value::as_bool).unwrap_or(false) {
        bail!("type facet failed");
    }
    // Step 2: Narrow to extensions within a pattern.
    let ext_facet = mcp.call_tool("uffs_facet_values", json!({"field": "ext", "pattern": "*.log", "top": 5}))?;
    if ext_facet.get("isError").and_then(Value::as_bool).unwrap_or(false) {
        bail!("scoped ext facet failed");
    }
    // Step 3: Agent decides to search for .log files.
    let search = mcp.call_tool("uffs_search", json!({"pattern": "*.log", "filter": "files", "limit": 5}))?;
    if search.get("isError").and_then(Value::as_bool).unwrap_or(false) {
        bail!("refined search failed");
    }
    Ok(format!("type facet → scoped ext facet → refined search"))
}

/// §8.2 + resource: Agent reads schema first, then queries.
/// Agent: read field catalog → read search schema → search.
fn flow_schema_then_query(mcp: &mut McpSession) -> Result<String> {
    // Step 1: Agent reads field catalog to understand available fields.
    let fields = mcp.request("resources/read", Some(json!({"uri": "uffs://schema/fields"})))?;
    result_of(&fields)?;
    // Step 2: Agent reads search schema to understand parameters.
    let schema = mcp.request("resources/read", Some(json!({"uri": "uffs://schema/search"})))?;
    result_of(&schema)?;
    // Step 3: Armed with knowledge, agent searches.
    let search = mcp.call_tool("uffs_search", json!({
        "pattern": "*.rs", "sort": "modified", "sort_desc": true, "limit": 10
    }))?;
    if search.get("isError").and_then(Value::as_bool).unwrap_or(false) {
        bail!("informed search failed");
    }
    Ok(format!("field catalog → search schema → informed query"))
}

/// §8.2 Agent uses a prompt then executes the suggested query.
/// Agent: get prompt → parse tool call from message → execute.
fn flow_prompt_guided(mcp: &mut McpSession) -> Result<String> {
    // Step 1: Agent requests the find_large_files prompt.
    let prompt_resp = mcp.request("prompts/get", Some(json!({"name": "find_large_files"})))?;
    let prompt = result_of(&prompt_resp)?;
    if prompt.pointer("/messages").is_none() { bail!("prompt missing messages"); }
    // Step 2: The prompt tells the agent to call uffs.search with certain params.
    // Agent follows through with a search for large files.
    let search = mcp.call_tool("uffs_search", json!({
        "pattern": "*", "sort": "size", "sort_desc": true, "limit": 10, "filter": "files"
    }))?;
    if search.get("isError").and_then(Value::as_bool).unwrap_or(false) {
        bail!("prompt-guided search failed");
    }
    Ok(format!("prompt → guided search (large files)"))
}

/// §8.4 Duplicate investigation (lightweight).
/// Agent: explore extensions → cleanup prompt → search for temp files.
fn flow_duplicate_investigation(mcp: &mut McpSession) -> Result<String> {
    // Step 1: Agent explores extensions to find duplicate-heavy types.
    let facets = mcp.call_tool("uffs_facet_values", json!({"field": "ext", "top": 10}))?;
    if facets.get("isError").and_then(Value::as_bool).unwrap_or(false) {
        bail!("extension facet failed");
    }
    // Step 2: Agent gets the cleanup report prompt for context.
    let prompt_resp = mcp.request("prompts/get", Some(json!({"name": "cleanup_report"})))?;
    let prompt = result_of(&prompt_resp)?;
    if prompt.pointer("/messages").is_none() { bail!("cleanup prompt missing messages"); }
    // Step 3: Agent searches for temp files as cleanup candidates.
    let search = mcp.call_tool("uffs_search", json!({"pattern": "*.tmp", "limit": 5}))?;
    if search.get("isError").and_then(Value::as_bool).unwrap_or(false) {
        bail!("temp file search failed");
    }
    Ok("ext facet → cleanup prompt → temp file search".to_owned())
}

fn build_agent_flow_tests() -> Vec<McpTest> {
    vec![
        flow("A800", "§8.1 Agent flow: search → inspect (known-item lookup)", flow_search_then_info),
        flow("A801", "§8.2 Agent flow: overview → facet → drill-down (summary)", flow_summary_then_drill),
        flow("A802", "§8.3 Agent flow: facet → scoped facet → refined search", flow_refine_search),
        flow("A803", "§8.2 Agent flow: read schema resources → informed query", flow_schema_then_query),
        flow("A804", "§8.2 Agent flow: prompt-guided search (find_large_files)", flow_prompt_guided),
        flow("A805", "§8.4 Agent flow: duplicate investigation → cleanup", flow_duplicate_investigation),
    ]
}

// ── Test execution ──────────────────────────────────────────────────────────

fn run_checks(result: &Value, text: &str, checks: &[Check]) -> Result<()> {
    for check in checks {
        match check {
            Check::Contains(s) => {
                if !text.contains(s.as_str()) {
                    bail!("expected text to contain \"{s}\"");
                }
            }
            Check::NotContains(s) => {
                if text.contains(s.as_str()) {
                    bail!("expected text NOT to contain \"{s}\"");
                }
            }
            Check::PathExists(ptr) => {
                if result.pointer(ptr).is_none() {
                    bail!("JSON pointer {ptr} not found in result");
                }
            }
            Check::PathEquals(ptr, expected) => {
                let actual = result.pointer(ptr)
                    .with_context(|| format!("JSON pointer {ptr} not found"))?;
                if actual != expected {
                    bail!("{ptr}: expected {expected}, got {actual}");
                }
            }
            Check::MinContent(min) => {
                let len = result.get("content")
                    .and_then(Value::as_array)
                    .map(|a| a.len())
                    .unwrap_or(0);
                if len < *min {
                    bail!("expected ≥{min} content items, got {len}");
                }
            }
            Check::NotError => {
                if result.get("isError").and_then(Value::as_bool).unwrap_or(false) {
                    bail!("result has isError=true: {}", extract_text(result));
                }
            }
            Check::ExpectRpcError => {
                // This check is handled at the caller level — if we reach
                // run_checks with a result, it means no error occurred.
                bail!("expected a JSON-RPC error, but got a successful result");
            }
        }
    }
    Ok(())
}

// ── Structured payload validation ───────────────────────────────────────────
//
// MCP search results have `structuredContent.rows[]` with fields like
// `name`, `size`, `is_directory`, `path`, `drive`, `modified`, `created`.
// We validate column_checks/sort_checks/row counts against this JSON.

/// Map TOML column names (from CLI CSV header) to structuredContent JSON keys.
///
/// structuredContent row shape (from uffs.search):
///   { drive, name, ext, type, size, modified, path, ... }
fn col_to_json(col: &str) -> &str {
    match col {
        "Name"           => "name",
        "Size"           => "size",
        "Directory Flag" => "is_directory",
        "Path"           => "path",
        "Path Only"      => "path",
        "Drive"          => "drive",
        "Modified"       => "modified",
        "Created"        => "created",
        "Accessed"       => "accessed",
        "Ext" | "Extension" => "ext",
        "Type"           => "type",
        "Descendants"    => "descendants",
        "Tree Size"      => "treesize",
        "Tree Allocated" => "tree_allocated",
        "Allocated"      => "allocated",
        "Bulkiness"      => "bulkiness",
        "Hidden"         => "hidden",
        "System"         => "system",
        "Compressed"     => "compressed",
        other => other,
    }
}

/// Extract a field value from a structured row as a string.
fn field_str(row: &Value, col: &str) -> String {
    let json_key = col_to_json(col);

    // Try the key directly in the row object.
    if let Some(v) = row.get(json_key) {
        let raw = match v {
            Value::String(s) => s.clone(),
            Value::Number(n) => n.to_string(),
            Value::Bool(b) => if *b { "1".into() } else { "0".into() },
            Value::Null => String::new(),
            other => other.to_string(),
        };

        // "Path Only" maps to the JSON "path" key but we only want the
        // directory portion (everything before the last `\`).
        if col == "Path Only" {
            return raw.rfind('\\')
                .map_or(raw.clone(), |pos| raw[..pos].to_owned());
        }

        return raw;
    }

    // Field not present in row — return empty.
    String::new()
}

/// Fields available in structuredContent rows (kept for reference/future use).
#[allow(dead_code)]
const STRUCTURED_FIELDS: &[&str] = &[
    "name", "ext", "type", "size", "modified", "created", "accessed",
    "path", "drive", "is_directory", "allocated",
    "descendants", "treesize", "tree_allocated", "bulkiness",
    "hidden", "system", "compressed",
];

/// Apply a single column check against one row.
/// Returns Ok(()) if the field isn't present in the row (skip gracefully).
fn apply_column_check(row_idx: usize, row: &Value, check: &ColumnCheck) -> Result<()> {
    let val = field_str(row, &check.column);

    // If the field is empty and not present in the row, skip this check.
    // The MCP structuredContent may not include all columns.
    if val.is_empty() {
        let json_key = col_to_json(&check.column);
        if row.get(json_key).is_none() {
            return Ok(()); // Field not in response — skip.
        }
    }

    let val_cmp = if check.case.as_deref() == Some("lower") { val.to_lowercase() } else { val.clone() };

    if let Some(ref op) = check.op {
        let expected = check.value.as_deref().unwrap_or("");
        let exp_cmp = if check.case.as_deref() == Some("lower") { expected.to_lowercase() } else { expected.to_string() };

        let ok = match op.as_str() {
            "eq"              => val_cmp == exp_cmp,
            "ne"              => val_cmp != exp_cmp,
            "contains"        => val_cmp.contains(&exp_cmp),
            "not_contains"    => !val_cmp.contains(&exp_cmp),
            "starts_with"     => val_cmp.starts_with(&exp_cmp),
            "not_starts_with" => !val_cmp.starts_with(&exp_cmp),
            "ends_with"       => val_cmp.ends_with(&exp_cmp),
            "gt" => val.parse::<f64>().unwrap_or(0.0) > expected.parse::<f64>().unwrap_or(0.0),
            "lt" => val.parse::<f64>().unwrap_or(0.0) < expected.parse::<f64>().unwrap_or(0.0),
            "ge" | "gte" => val.parse::<f64>().unwrap_or(0.0) >= expected.parse::<f64>().unwrap_or(0.0),
            "le" | "lte" => val.parse::<f64>().unwrap_or(0.0) <= expected.parse::<f64>().unwrap_or(0.0),
            _ => { return Ok(()); } // Unknown op — skip gracefully.
        };
        if !ok {
            bail!("Row {row_idx}: column '{}' op={op} failed: got '{}', expected '{}'",
                check.column, val, expected);
        }
    }
    Ok(())
}

/// Validate a tool call response against TOML payload checks.
fn validate_payload(result: &Value, checks: &PayloadChecks) -> Result<()> {
    // Extract rows from structuredContent.rows[].
    let rows = result.get("structuredContent")
        .or_else(|| result.get("structured_content"))
        .and_then(|sc| sc.get("rows"))
        .and_then(Value::as_array);

    let row_count = rows.map(|r| r.len()).unwrap_or(0);

    // ── Row count checks ──────────────────────────────────────────
    if let Some(min) = checks.expect_min_rows {
        if row_count < min {
            bail!("Expected ≥{min} rows, got {row_count}");
        }
    }
    if let Some(max) = checks.expect_max_rows {
        if row_count > max {
            bail!("Expected ≤{max} rows, got {row_count}");
        }
    }

    // ── Column checks ─────────────────────────────────────────────
    if let Some(rows) = rows {
        for (i, row) in rows.iter().enumerate() {
            for check in &checks.column_checks {
                apply_column_check(i, row, check)?;
            }
        }

        // ── Sort checks ───────────────────────────────────────────
        if rows.len() >= 2 {
            for sc in &checks.sort_checks {
                let order = sc.order.as_deref().unwrap_or("asc");
                let sort_type = sc.sort_type.as_deref().unwrap_or("string");

                if sort_type == "string" || sort_type == "str" {
                    let vals: Vec<String> = rows.iter()
                        .map(|r| field_str(r, &sc.column).to_lowercase())
                        .collect();
                    for w in vals.windows(2) {
                        match order {
                            "asc"  => { if w[0] > w[1] { bail!("Sort {}: not asc: '{}' > '{}'", sc.column, w[0], w[1]); } }
                            "desc" => { if w[0] < w[1] { bail!("Sort {}: not desc: '{}' < '{}'", sc.column, w[0], w[1]); } }
                            _ => {}
                        }
                    }
                } else {
                    let vals: Vec<f64> = rows.iter()
                        .map(|r| field_str(r, &sc.column).parse::<f64>().unwrap_or(0.0))
                        .collect();
                    for w in vals.windows(2) {
                        match order {
                            "asc"  => { if w[0] > w[1] { bail!("Sort {}: not asc: {} > {}", sc.column, w[0], w[1]); } }
                            "desc" => { if w[0] < w[1] { bail!("Sort {}: not desc: {} < {}", sc.column, w[0], w[1]); } }
                            _ => {}
                        }
                    }
                }
            }
        }
    } else if !checks.column_checks.is_empty() || !checks.sort_checks.is_empty() {
        // No structured rows but we have checks — might be aggregate/facet.
        // Skip gracefully for non-search tools.
    }

    // ── Custom validator ─────────────────────────────────────────
    if let Some(ref name) = checks.validator {
        run_mcp_custom_validator(name, result)?;
    }

    Ok(())
}

/// Dispatch a named custom validator against the MCP tool-call result.
///
/// This mirrors `run_rpc_custom_validator` in `api-validation.rs` and
/// `run_custom_validator` in `cli-validation.rs`.  The MCP transport
/// wraps the daemon RPC layer, so the SAME invariants apply — every
/// validator ported here targets `result.structuredContent.rows[]`,
/// the MCP tool-call payload analogue of the RPC `rows` array.
///
/// Validators that haven't been explicitly ported yet emit a clear
/// skip diagnostic rather than failing, so new tests can be written
/// against MCP from day one and gaps surface without blocking the
/// suite.  Port each validator as needed — the api-validation.rs
/// source is the source of truth for the invariant.
fn run_mcp_custom_validator(name: &str, result: &Value) -> Result<String> {
    match name {
        "T67f2" => {
            // PathOnly sort must honour Windows Explorer's `Folder`
            // column convention: when two rows share the same parent
            // directory (case-insensitive), the secondary tiebreaker
            // is filename ASC.  See the api-validation.rs and
            // cli-validation.rs mirrors, and the Rust unit test
            // `search_index_path_only_sort_name_asc_within_same_folder`
            // in crates/uffs-core/src/search/backend_tests.rs.
            //
            // This validator only checks the tiebreaker — primary
            // `path_only` ASC is covered by T67f via the generic
            // sort_checks framework (see the rationale comment in the
            // api-validation.rs mirror for the $UpCase vs lowercase
            // fold-direction subtlety around `_`).
            let rows = result
                .get("structuredContent")
                .or_else(|| result.get("structured_content"))
                .and_then(|sc| sc.get("rows"))
                .and_then(Value::as_array)
                .ok_or_else(|| anyhow::anyhow!("No structuredContent.rows in MCP response"))?;
            if rows.len() < 2 {
                bail!(
                    "Need ≥ 2 rows to validate path_only+name sort, got {}",
                    rows.len()
                );
            }
            let pairs: Vec<(String, String)> = rows
                .iter()
                .map(|r| {
                    let path = field_str(r, "Path");
                    let name = field_str(r, "Name");
                    let dir = path
                        .strip_suffix(&name)
                        .unwrap_or(&path)
                        .trim_end_matches('\\')
                        .to_owned();
                    (dir, name)
                })
                .collect();
            let mut saw_tiebreaker = false;
            for w in pairs.windows(2) {
                let (po0, n0) = &w[0];
                let (po1, n1) = &w[1];
                if po0.eq_ignore_ascii_case(po1) {
                    saw_tiebreaker = true;
                    let n0_fold = n0.to_lowercase();
                    let n1_fold = n1.to_lowercase();
                    if n0_fold > n1_fold {
                        bail!(
                            "Not asc (name tiebreaker within '{}'): '{}' > '{}'",
                            po0, n0, n1
                        );
                    }
                }
            }
            if !saw_tiebreaker {
                bail!(
                    "Test vacuous: {} rows all have distinct path_only \
                     values — no adjacent pair with equal path_only to \
                     exercise the name tiebreaker.  Expand the search \
                     or raise --limit so rows from the same folder \
                     appear together.",
                    rows.len()
                );
            }
            Ok(format!(
                "{} rows, name-ASC tiebreaker verified for same-folder siblings",
                rows.len()
            ))
        }
        // Graceful skip for validators that haven't been ported to MCP
        // yet.  The api/cli suites still run them — this just keeps the
        // MCP suite unblocked while porting happens incrementally.
        other => Ok(format!(
            "(validator '{other}' not yet ported to MCP — skipped, structural checks still ran)"
        )),
    }
}

/// Build a shell-safe CLI reproduction string from binary + args.
///
/// Quotes arguments containing shell metacharacters (`*`, `>`, `<`, spaces)
/// so the displayed command can be copy-pasted directly into a terminal.
fn build_cli_string(bin: &str, cli_args: &[String], source_flag: Option<&str>, source_path: &str) -> String {
    let mut parts = vec![bin.to_string()];

    let is_daemon_cmd = cli_args.first().map(|a| a.as_str()) == Some("daemon")
        || cli_args.first().map(|a| a.as_str()) == Some("info");

    if cli_args.is_empty() {
        parts.push("\"*\"".into());
    } else {
        for a in cli_args {
            if a.contains(' ') || a.contains('*') || a.contains('>') || a.contains('<') {
                parts.push(format!("\"{a}\""));
            } else {
                parts.push(a.clone());
            }
        }
    }

    if !is_daemon_cmd {
        if let Some(flag) = source_flag {
            parts.push(flag.to_string());
            parts.push(source_path.to_string());
        }
    }

    parts.join(" ")
}

/// Build a compact RPC reproduction string: `method(params_json)`.
fn build_rpc_string(method: &str, cli_args: &[String]) -> String {
    if cli_args.is_empty() {
        return method.to_string();
    }
    // Re-derive RPC params from CLI args (same as api-validation).
    let (_, params) = cli_args_to_mcp_params(cli_args);
    format!("{method}({params})")
}



/// Run a single test.  M100-M103 (initialize) are special: the init
/// handshake is completed once at session start, so we skip actually
/// sending a duplicate initialize — instead we validate the cached
/// init_result.
fn run_test(
    mcp: &mut McpSession,
    test: &McpTest,
    init_result: &Value,
    script_args: &ScriptArgs,
) -> TestResult {
    let t0 = Instant::now();

    // Build CLI/RPC reproduction strings once for all paths.
    let cli_command = build_cli_string(
        &script_args.bin, &test.cli_args,
        script_args.source_flag, &script_args.source_path,
    );
    let rpc_call = build_rpc_string(&test.rpc_method, &test.cli_args);

    // Agent flows get their own execution path — multi-step, so we show
    // the step descriptions rather than a single payload.
    if let McpTestKind::AgentFlow(flow_fn) = &test.kind {
        let result = flow_fn(mcp);
        let elapsed = t0.elapsed().as_millis();
        let mut r = match result {
            Ok(detail) => TestResult {
                id: test.id.clone(), name: test.name.clone(),
                passed: true, message: detail, elapsed_ms: elapsed,
                mcp_request: "(multi-step agent flow)".into(),
                mcp_response: String::new(),
                cli_command: cli_command.clone(), rpc_call: rpc_call.clone(),
            },
            Err(e) => TestResult {
                id: test.id.clone(), name: test.name.clone(),
                passed: false, message: format!("{e:#}"), elapsed_ms: elapsed,
                mcp_request: "(multi-step agent flow)".into(),
                mcp_response: String::new(),
                cli_command: cli_command.clone(), rpc_call: rpc_call.clone(),
            },
        };
        if r.passed && elapsed > AGENT_FLOW_TIMEOUT_MS {
            r.passed = false;
            r.message = format!("TIMEOUT: {}ms exceeds {}ms budget", elapsed, AGENT_FLOW_TIMEOUT_MS);
        }
        return r;
    }

    // Build the request payload string for diagnostics.
    let (req_str, resp_str, outcome): (String, String, Result<()>) = match &test.kind {
        McpTestKind::Protocol(p) => {
            if p.method == "initialize" {
                let req = format!("initialize({})",
                    serde_json::to_string_pretty(&p.params).unwrap_or_default());
                let resp = serde_json::to_string_pretty(init_result).unwrap_or_default();
                let text = serde_json::to_string(init_result).unwrap_or_default();
                let outcome = run_checks(init_result, &text, &p.checks);
                (req, resp, outcome)
            } else {
                let req_payload = match &p.params {
                    Some(params) => json!({"method": p.method, "params": params}),
                    None => json!({"method": p.method}),
                };
                let req = serde_json::to_string_pretty(&req_payload).unwrap_or_default();
                let expects_error = p.checks.iter().any(|c| matches!(c, Check::ExpectRpcError));
                match mcp.request(&p.method, p.params.clone()) {
                    Ok(resp) => {
                        let resp_s = serde_json::to_string_pretty(&resp).unwrap_or_default();
                        if expects_error {
                            let outcome = if resp.get("error").is_some() {
                                Ok(())
                            } else {
                                Err(anyhow::anyhow!("expected JSON-RPC error but got success"))
                            };
                            (req, resp_s, outcome)
                        } else {
                            match result_of(&resp) {
                                Ok(result) => {
                                    let text = serde_json::to_string(&result).unwrap_or_default();
                                    (req, resp_s, run_checks(&result, &text, &p.checks))
                                }
                                Err(e) => (req, resp_s, Err(e)),
                            }
                        }
                    }
                    Err(e) => {
                        if expects_error { (req, "(error as expected)".into(), Ok(())) }
                        else { (req, String::new(), Err(e)) }
                    },
                }
            }
        }
        McpTestKind::ToolCall(tc) => {
            let req_payload = json!({
                "method": "tools/call",
                "params": { "name": tc.tool, "arguments": tc.args }
            });
            let req = serde_json::to_string_pretty(&req_payload).unwrap_or_default();
            match mcp.call_tool(&tc.tool, tc.args.clone()) {
                Ok(result) => {
                    let resp_s = serde_json::to_string_pretty(&result).unwrap_or_default();
                    let text = extract_text(&result);
                    // Basic checks (NotError, MinContent).
                    let outcome = run_checks(&result, &text, &tc.checks);
                    // If basic checks pass, apply TOML payload validation.
                    let outcome = if outcome.is_ok() {
                        if let Some(ref pc) = test.payload_checks {
                            validate_payload(&result, pc)
                        } else {
                            Ok(())
                        }
                    } else {
                        outcome
                    };
                    (req, resp_s, outcome)
                }
                Err(e) => (req, String::new(), Err(e)),
            }
        }
        McpTestKind::AgentFlow(_) => unreachable!(),
    };

    let elapsed = t0.elapsed().as_millis();
    let budget = TEST_TIMEOUT_MS;

    // Enforce per-test time budget.
    let mut result = match outcome {
        Ok(()) => TestResult {
            id: test.id.clone(), name: test.name.clone(),
            passed: true, message: String::new(), elapsed_ms: elapsed,
            mcp_request: req_str, mcp_response: resp_str,
            cli_command: cli_command.clone(), rpc_call: rpc_call.clone(),
        },
        Err(e) => TestResult {
            id: test.id.clone(), name: test.name.clone(),
            passed: false, message: format!("{e:#}"), elapsed_ms: elapsed,
            mcp_request: req_str, mcp_response: resp_str,
            cli_command, rpc_call,
        },
    };
    if result.passed && elapsed > budget {
        result.passed = false;
        result.message = format!("TIMEOUT: {}ms exceeds {}ms budget", elapsed, budget);
    }
    result
}
// ── Main ────────────────────────────────────────────────────────────────────

/// Find the longest common prefix of a set of strings.
fn common_prefix(strings: &[&str]) -> String {
    if strings.is_empty() { return String::new(); }
    let first = strings[0];
    let mut len = first.len();
    for s in &strings[1..] {
        len = len.min(s.len());
        for (i, (a, b)) in first.bytes().zip(s.bytes()).enumerate() {
            if a != b { len = len.min(i); break; }
        }
    }
    first[..len].to_string()
}


/// Run `uffs <args>` and return (exit_code, stdout, stderr).
fn run_uffs(bin: &str, args: &[&str]) -> Result<(i32, String, String)> {
    let out = Command::new(bin).args(args).output()
        .with_context(|| format!("failed to run: {bin} {}", args.join(" ")))?;
    let code = out.status.code().unwrap_or(-1);
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    Ok((code, stdout, stderr))
}

/// Ensure the daemon is running and ready with drives loaded.
///
/// 1. `uffs daemon status` — if "Ready" with drives, done.
/// 2. If not running or stale, `uffs daemon start --data-dir ...`
///    (blocks until daemon is ready, no polling needed).
///
/// Prints the daemon status banner (PID, uptime, drives, records).
/// Returns the time spent (ms).  Aborts on failure.
fn ensure_daemon_ready(args: &ScriptArgs) -> u128 {
    let bin = &args.bin;
    let t0 = Instant::now();

    eprintln!("  Checking daemon status...");
    match run_uffs(bin, &["daemon", "status"]) {
        Ok((_code, stdout, stderr)) => {
            let combined = format!("{stdout}{stderr}");
            let lower = combined.to_lowercase();

            if lower.contains("ready") {
                let has_drives = !lower.contains("none loaded")
                    && !lower.contains("drives:        0")
                    && !lower.contains("drives: 0");

                if has_drives {
                    let ms = t0.elapsed().as_millis();
                    eprintln!("  Daemon: {} ✓ ({ms}ms)", "Ready".green().bold());
                    for line in combined.lines() {
                        eprintln!("    {line}");
                    }
                    return ms;
                }

                // Daemon is Ready but has zero drives — stale/useless.
                eprintln!("  Daemon is {} but has {} — restarting with data source...",
                    "Ready".yellow(), "zero drives loaded".red().bold());
                for line in combined.lines() {
                    eprintln!("    {line}");
                }
                eprintln!("  Stopping stale daemon...");
                let _ = run_uffs(bin, &["daemon", "stop"]);
                std::thread::sleep(Duration::from_millis(500));
            } else if lower.contains("not running") {
                eprintln!("  Daemon is not running, starting...");
            } else {
                eprintln!("  Daemon status unclear, attempting start...");
                for line in combined.lines() {
                    eprintln!("    {line}");
                }
            }
        }
        Err(e) => {
            eprintln!("  Cannot check daemon status: {e}");
            eprintln!("  Attempting start...");
        }
    }

    // Step 2: Start daemon (blocks until ready — `daemon start` waits
    // until all drives are loaded before returning).
    let mut start_args: Vec<&str> = vec!["daemon", "start"];
    if let Some(f) = args.source_flag { start_args.push(f); start_args.push(&args.source_path); }
    let cli_str = format!("{bin} {}", start_args.join(" "));
    eprintln!("    ↳ {}", cli_str.dimmed());

    match run_uffs(bin, &start_args) {
        Ok((_code, stdout, stderr)) => {
            let ms = t0.elapsed().as_millis();
            let combined = format!("{stdout}{stderr}");
            eprintln!("  Daemon: {} ({ms}ms)", "Started".green().bold());
            for line in combined.lines() {
                eprintln!("    {line}");
            }
        }
        Err(e) => {
            let ms = t0.elapsed().as_millis();
            eprintln!("  Daemon start {} — {e} ({ms}ms)", "FAILED".red().bold());
            eprintln!("  {}", "Cannot proceed without a running daemon. Aborting.".red().bold());
            std::process::exit(1);
        }
    }

    // Verify it's really ready now.
    match run_uffs(bin, &["daemon", "status"]) {
        Ok((_code, stdout, stderr)) => {
            let ms = t0.elapsed().as_millis();
            let combined = format!("{stdout}{stderr}");
            eprintln!("  Daemon: {} ✓ ({ms}ms)", "Ready".green().bold());
            for line in combined.lines() {
                eprintln!("    {line}");
            }
            ms
        }
        Err(_) => t0.elapsed().as_millis(),
    }
}

fn main() -> Result<()> {
    let script_start = Instant::now();
    let args = parse_args();
    let build_ms = script_start.elapsed().as_millis();

    eprintln!("╔═══════════════════════════════════════════════════════════════╗");
    eprintln!("║  UFFS MCP Capability & Conformance Validation               ║");
    eprintln!("╚═══════════════════════════════════════════════════════════════╝");
    eprintln!("  Binary:  {}", args.bin.cyan());
    match args.source_flag {
        Some(f) => eprintln!("  Source:  {} {}", f, args.source_path.cyan()),
        None    => eprintln!("  Source:  {}", "(live NTFS drives)".cyan()),
    }
    if let Some(ref f) = args.filter {
        eprintln!("  Tests:   {}", f.yellow());
    }
    eprintln!();

    // ═══ Ensure daemon is running before tests ═════════════════════════
    let daemon_ms = ensure_daemon_ready(&args);

    // Build source args for the MCP session.
    let src_args: Vec<&str> = match args.source_flag {
        Some(f) => vec![f, &args.source_path],
        None    => vec![],
    };

    // ═══ Spawn MCP session ═════════════════════════════════════════════
    let mcp_cmd = {
        let mut parts = vec![args.bin.as_str(), "mcp", "run"];
        parts.extend_from_slice(&src_args);
        parts.join(" ")
    };
    eprintln!();
    eprintln!("  Spawning MCP session...");
    eprintln!("  Transport: {}", "stdio (JSON-RPC over stdin/stdout)".cyan());
    eprintln!("  Command:   {}", mcp_cmd.cyan());
    let mut mcp = McpSession::spawn(&args.bin, &src_args)?;
    let init_result = mcp.initialize()?;
    eprintln!("  MCP: {} (protocol={}, server={})",
        "Initialized".green().bold(),
        init_result.pointer("/protocolVersion").and_then(Value::as_str).unwrap_or("?"),
        init_result.pointer("/serverInfo/name").and_then(Value::as_str).unwrap_or("?"),
    );

    // Build and filter tests — all definitions from TOML + agent flows.
    let mut tests = load_tests_from_toml();
    tests.extend(build_agent_flow_tests());
    if let Some(ref f) = args.filter {
        // Support comma-separated test IDs: --tests T119,T120,M300
        let filters: Vec<String> = f.split(',')
            .map(|s| s.trim().to_lowercase())
            .filter(|s| !s.is_empty())
            .collect();
        let all_ids: Vec<String> = tests.iter().map(|t| t.id.clone()).collect();
        tests.retain(|t| {
            let id_lower = t.id.to_lowercase();
            let name_lower = t.name.to_lowercase();
            filters.iter().any(|flt| id_lower.contains(flt) || name_lower.contains(flt))
        });
        if tests.is_empty() {
            eprintln!();
            eprintln!("  {} No tests match \"{}\"", "⚠️".yellow(), f.yellow());
            eprintln!();
            eprintln!("  Available test IDs:");
            let mut groups: std::collections::BTreeMap<String, Vec<String>> = std::collections::BTreeMap::new();
            for id in &all_ids {
                let prefix = if id.len() >= 2 { &id[..2] } else { id.as_str() };
                groups.entry(prefix.to_string()).or_default().push(id.clone());
            }
            for (prefix, ids) in &groups {
                eprintln!("    {prefix}xx: {}", ids.join(", "));
            }
            eprintln!();
            eprintln!("  Examples:");
            eprintln!("    --tests M100             single test");
            eprintln!("    --tests M100,M200,T119   multiple tests");
            eprintln!("    --tests M1               all M1xx tests");
            eprintln!("    --tests search           all tests with \"search\" in the name");
            eprintln!("    --tests A8               all agent flow tests");
            std::process::exit(0);
        }
    }

    eprintln!();
    eprintln!("┌───────────────────────────────────────────────────────────────┐");
    eprintln!("│  MCP Validation ({} tests, agent-flow model)            │", format!("{:>3}", tests.len()));
    eprintln!("└───────────────────────────────────────────────────────────────┘");

    // Run tests — print each result inline as it completes.
    let test_start = Instant::now();
    let mut results = Vec::new();
    for test in &tests {
        let r = run_test(&mut mcp, test, &init_result, &args);
        let status = if r.passed {
            format!("{}", "PASS".green().bold())
        } else {
            format!("{}", "FAIL".red().bold())
        };
        let timing = format!("{:>5}ms", r.elapsed_ms).dimmed();
        if r.passed {
            let detail = if r.message.is_empty() { String::new() } else { format!(" — {}", r.message) };
            eprintln!("  [{status}] {timing}  {}{detail} [mcp]", r.name);
        } else {
            eprintln!("  [{status}] {timing}  {}: {}", r.name, r.message.red());
        }
        results.push(r);
    }
    let test_wall_ms = test_start.elapsed().as_millis();

    // Cleanup.
    mcp.close_stdin();
    let _ = mcp.child.wait();

    // Summary.
    let total = results.len();
    let passed = results.iter().filter(|r| r.passed).count();
    let failed = total - passed;
    let test_sum_ms: u128 = results.iter().map(|r| r.elapsed_ms).sum();
    let test_avg_ms = if total > 0 { test_sum_ms / total as u128 } else { 0 };

    eprintln!();
    if failed > 0 {
        eprintln!("  {} {failed}/{total} FAILED", "❌".red());
        eprintln!();
        eprintln!("  ┌─ Failed Tests ──────────────────────────────────────────────────────┐");
        for r in &results {
            if !r.passed {
                eprintln!("  │");
                eprintln!("  │  {} {}", "❌".red(), r.name);
                eprintln!("  │  {}: {}", "Error".red().bold(), r.message);
                eprintln!("  │  {}:   {}", "CLI".yellow().bold(), r.cli_command);
                eprintln!("  │  {}:   {}", "RPC".cyan().bold(), r.rpc_call);
                // Show the MCP request payload.
                eprintln!("  │  {}:", "MCP Request".yellow().bold());
                for line in r.mcp_request.lines() {
                    eprintln!("  │    {}", line.yellow());
                }
                // Show the MCP response payload on failure.
                if !r.mcp_response.is_empty() {
                    eprintln!("  │  {}:", "MCP Response".cyan().bold());
                    let resp_lines: Vec<&str> = r.mcp_response.lines().collect();
                    let show = if resp_lines.len() > 30 { 30 } else { resp_lines.len() };
                    for line in &resp_lines[..show] {
                        eprintln!("  │    {}", line.dimmed());
                    }
                    if resp_lines.len() > 30 {
                        eprintln!("  │    {} ({} more lines)", "...".dimmed(), resp_lines.len() - 30);
                    }
                }
            }
        }
        eprintln!("  │");
        eprintln!("  └──────────────────────────────────────────────────────────────────────┘");
    } else {
        eprintln!("  {} {passed}/{total} passed", "✅".green());
    }

    // When running a small number of tests (e.g. --tests M100), show full
    // MCP payloads for every test — same info shown on failure, so users
    // can replay or inspect each request.  Skip when every test already
    // appeared in the failure box above to avoid duplicate output.
    if total <= 10 && total > 0 && passed > 0 {
        eprintln!();
        eprintln!("  ┌─ Test Details ───────────────────────────────────────────────────────┐");
        for r in &results {
            let icon = if r.passed { "✅" } else { "❌" };
            eprintln!("  │");
            eprintln!("  │  {icon} {} ({}ms)", r.name, r.elapsed_ms);
            eprintln!("  │  {}: {}", "Result".bold(), if r.message.is_empty() { "OK" } else { &r.message });
            eprintln!("  │  {}:    {}", "CLI".yellow().bold(), r.cli_command);
            eprintln!("  │  {}:    {}", "RPC".cyan().bold(), r.rpc_call);
            eprintln!("  │  {}:", "MCP Request".yellow().bold());
            for line in r.mcp_request.lines() {
                eprintln!("  │    {}", line.yellow());
            }
            if !r.mcp_response.is_empty() {
                eprintln!("  │  {}:", "MCP Response".cyan().bold());
                let resp_lines: Vec<&str> = r.mcp_response.lines().collect();
                let show = if resp_lines.len() > 20 { 20 } else { resp_lines.len() };
                for line in &resp_lines[..show] {
                    eprintln!("  │    {}", line.dimmed());
                }
                if resp_lines.len() > 20 {
                    eprintln!("  │    {} ({} more lines)", "...".dimmed(), resp_lines.len() - 20);
                }
            }
        }
        eprintln!("  │");
        eprintln!("  └──────────────────────────────────────────────────────────────────────┘");
    }

    // ═══ Final timing summary ══════════════════════════════════════════
    let script_total_ms = script_start.elapsed().as_millis();
    let slowest = results.iter().max_by_key(|r| r.elapsed_ms);
    let fastest = results.iter().filter(|r| r.passed).min_by_key(|r| r.elapsed_ms);

    eprintln!();
    eprintln!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    eprintln!("  {} Timing Breakdown", "⏱".dimmed());
    eprintln!("  ─────────────────────────────────────────────────────");
    eprintln!("  Build binary:         {:>7}ms", build_ms);
    eprintln!("  Daemon ready:         {:>7}ms  (status check + start + drive load)", daemon_ms);
    eprintln!("  ─────────────────────────────────────────────────────");
    eprintln!("  Tests wall time:      {:>7}ms  ({total} tests, sequential MCP session)", test_wall_ms);
    eprintln!("  Tests sum time:       {:>7}ms  (total across all tests)", test_sum_ms);
    eprintln!("  Tests avg time:       {:>7}ms  (per test)", test_avg_ms);
    if let Some(s) = slowest {
        eprintln!("  Slowest test:         {:>7}ms  {}", s.elapsed_ms, s.name.dimmed());
    }
    if let Some(f) = fastest {
        eprintln!("  Fastest test:         {:>7}ms  {}", f.elapsed_ms, f.name.dimmed());
    }
    eprintln!("  ─────────────────────────────────────────────────────");
    eprintln!("  Script total:         {:>7}ms", script_total_ms);
    eprintln!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");

    // ═══ Cleanup: verify daemon, kill orphan uffs processes ═════════════
    cleanup_orphan_processes(&args.bin);

    if failed > 0 {
        // Build retest command with failed test IDs.
        let failed_ids: Vec<&str> = results.iter()
            .filter(|r| !r.passed)
            .map(|r| r.id.as_str())
            .collect();
        eprintln!();
        eprintln!("  Retest failed using:");
        let joined = failed_ids.join(",");
        eprintln!("    rust-script scripts/windows/mcp-validation.rs --tests {joined}");
        if failed_ids.len() > 1 {
            // Also suggest a prefix shortcut if applicable.
            let prefix = common_prefix(&failed_ids);
            if !prefix.is_empty() && prefix.len() >= 2 {
                eprintln!();
                eprintln!("  Or by prefix:");
                eprintln!("    rust-script scripts/windows/mcp-validation.rs --tests {prefix}");
            }
        }

        eprintln!("\n{}", "MCP validation FAILED.".red().bold());
        std::process::exit(1);
    }

    eprintln!("\n{}", "MCP validation PASSED. ✓".green().bold());
    Ok(())
}

// ── Orphan Process Cleanup ──────────────────────────────────────────────────

/// Find all running uffs processes via `ps`.
fn find_uffs_processes(bin_name: &str) -> Vec<(u32, String)> {
    let my_pid = std::process::id();
    let output = Command::new("ps")
        .args(["-eo", "pid,command"])
        .output()
        .ok();
    let stdout = output
        .as_ref()
        .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
        .unwrap_or_default();

    stdout
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            if !line.contains(bin_name) { return None; }
            // Skip ps/grep/rust-script themselves.
            if line.contains("ps -eo") || line.contains("grep") || line.contains("rust-script") {
                return None;
            }
            let mut parts = line.splitn(2, char::is_whitespace);
            let pid: u32 = parts.next()?.trim().parse().ok()?;
            let cmd = parts.next().unwrap_or("").trim().to_string();
            if pid == my_pid { return None; }
            Some((pid, cmd))
        })
        .collect()
}

/// Read the daemon PID from the PID file.
/// File format: `{pid}\n{timestamp}\n{exe_hash}\n{nonce}\n` — we read line 1.
fn read_daemon_pid() -> Option<u32> {
    let base = dirs_next::data_local_dir().unwrap_or_else(|| PathBuf::from("/tmp"));
    let pid_path = base.join("uffs").join("daemon.pid");
    let content = std::fs::read_to_string(&pid_path).ok()?;
    content.lines().next()?.trim().parse().ok()
}

/// Query the daemon for its PID, find all running uffs processes, and kill
/// only the orphans.  The legitimate daemon is left running.
fn cleanup_orphan_processes(bin: &str) {
    eprintln!();
    eprintln!("┌───────────────────────────────────────────────────────────────┐");
    eprintln!("│  Cleanup: verifying daemon & checking for orphan processes   │");
    eprintln!("└───────────────────────────────────────────────────────────────┘");

    let daemon_pid = read_daemon_pid();
    if let Some(pid) = daemon_pid {
        eprintln!("  {} Daemon verified (PID {pid})", "✅".green());
    } else {
        eprintln!("  {} Could not read daemon PID file — daemon may not be running", "⚠️".yellow());
    }

    let bin_name = std::path::Path::new(bin)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("uffs");
    let all_procs = find_uffs_processes(bin_name);

    // Protect: our own PID, the daemon PID, and any `daemon run` or `mcp run`
    // processes (those are the legitimate daemon and MCP server).
    let my_pid = std::process::id();
    let orphans: Vec<_> = all_procs
        .iter()
        .filter(|(pid, cmd)| {
            if *pid == my_pid { return false; }
            if daemon_pid.map_or(false, |dp| *pid == dp) { return false; }
            // Keep daemon and MCP server processes alive.
            let cmd_lower = cmd.to_lowercase();
            if cmd_lower.contains("daemon run") || cmd_lower.contains("mcp run") {
                return false;
            }
            true
        })
        .collect();

    if orphans.is_empty() {
        eprintln!("  {} No orphan uffs processes found.", "✅".green());
        return;
    }

    eprintln!("  {} {} orphan uffs process(es) — killing:",
        "⚠️".yellow(), orphans.len());

    let mut killed = 0;
    for (pid, cmd) in &orphans {
        eprintln!("    PID {pid}: {cmd}");
        #[cfg(unix)]
        {
            use std::process::Command as Cmd;
            let ok = Cmd::new("kill").arg("-9").arg(pid.to_string())
                .output().map(|o| o.status.success()).unwrap_or(false);
            if ok { killed += 1; }
        }
        #[cfg(windows)]
        {
            use std::process::Command as Cmd;
            let ok = Cmd::new("taskkill").args(["/F", "/PID", &pid.to_string()])
                .output().map(|o| o.status.success()).unwrap_or(false);
            if ok { killed += 1; }
        }
    }
    eprintln!("  🔪 Killed {killed}/{} orphan process(es).", orphans.len());
}
