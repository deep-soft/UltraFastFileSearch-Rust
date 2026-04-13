# Security Policy

## Supported Versions

| Version | Supported |
|---------|-----------|
| latest  | ✅        |
| < latest | ❌       |

Only the latest release receives security updates. We recommend always running
the most recent version.

## Reporting a Vulnerability

**Do NOT open a public issue for security vulnerabilities.**

If you discover a security vulnerability in UFFS, please report it responsibly
through one of these channels:

1. **GitHub Security Advisories (preferred)**
   → [Report a vulnerability](https://github.com/githubrobbi/Ultra-Fast-File-Search/security/advisories/new)

2. **Email**
   → Send details to the email address listed in the repository's `Cargo.toml`
   under `[workspace.package]`.

### What to include

- A description of the vulnerability and its potential impact
- Steps to reproduce or a proof-of-concept
- Affected versions (if known)
- Any suggested fix or mitigation

### What to expect

| Step | Timeline |
|------|----------|
| Acknowledgement | Within **48 hours** |
| Initial assessment | Within **7 days** |
| Fix + advisory published | Within **30 days** (critical) / **90 days** (other) |

We will credit reporters in the advisory unless anonymity is requested.

## Scope

This policy covers:

- The `uffs` and `uffs_mft` binaries
- All crates in the `crates/` workspace
- The `uffs-daemon` service and its JSON-RPC / MCP interfaces
- Index files written to disk (`.uffs-index`)
- Build and CI infrastructure (GitHub Actions workflows)

### Out of scope

- The public C++ predecessor repository
  (`github.com/githubrobbi/Ultra-Fast-File-Search-CPP`)
- Third-party dependencies (report upstream; we monitor via `cargo deny` and
  Dependabot)

## Security Measures

This project maintains the following security practices:

- **Signed commits** — All commits are cryptographically signed (GPG/Ed25519)
- **Dependency auditing** — `cargo deny check` runs on every PR
  (advisories, licenses, bans, sources)
- **Automated dependency updates** — Dependabot monitors Cargo and GitHub
  Actions dependencies
- **CI action pinning** — All GitHub Actions are pinned to immutable commit
  SHAs to prevent supply chain attacks
- **Strict Clippy** — `unsafe_code = "deny"`, `unwrap_used = "deny"`,
  `expect_used = "deny"` enforced workspace-wide
- **No unsafe code** — Zero `unsafe` blocks in production code without
  explicit `#[allow(unsafe_code)]` and safety documentation
- **Least-privilege CI** — Workflows use `permissions: contents: read`
- **SPDX compliance** — Every source file carries
  `SPDX-License-Identifier: MPL-2.0`
