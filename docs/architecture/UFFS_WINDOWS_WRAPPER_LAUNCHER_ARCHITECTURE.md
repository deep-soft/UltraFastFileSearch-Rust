# UFFS Windows Wrapper Launcher Architecture

Status: Proposed
Date: 2026-03-31
Audience: UFFS maintainers, release engineering, security engineering, CI/CD owners
Primary target: Windows x64 (`x86_64-pc-windows-msvc`)
Secondary target: Windows ARM64 (`aarch64-pc-windows-msvc`)

---

## 1. Executive summary

This document proposes a production-grade distribution architecture for UFFS on Windows that reduces the shipped download size without changing the runtime trust model of the real application binary.

The design is intentionally **not** an in-memory PE loader and **not** a classic PE packer. Instead, it uses:

1. A **small signed launcher** (`uffs.exe`) that the user downloads and starts.
2. A **separately signed real payload binary** (`uffs-real.exe`) produced by the normal Rust build.
3. A **compressed copy of the signed payload** embedded into or appended to the launcher package.
4. A **secure first-run extraction** into a per-user protected directory.
5. A **fast steady-state path** where later launches skip decompression and directly execute the already extracted payload after local integrity checks.

In short:

- Shipping artifact is smaller than the raw payload binary.
- The user still sees a single executable entry point.
- First run may pay a small extraction cost.
- Subsequent launches are fast.
- The launcher remains stable in place.
- The extracted runtime lives in a secure versioned/hash-addressed location.
- Both the launcher and the inner runtime are signed.
- The launcher binds itself to the exact inner payload via a baked-in manifest and content hash.

**Recommended production decision:** build a **small custom UFFS launcher** and a **small UFFS packager tool**, but use mature crates and platform APIs for the hard parts. Use off-the-shelf packers only for benchmarking or rapid prototypes, not as the long-term security boundary.

---

## 2. Why this architecture exists

UFFS already has a serious security posture:

- secure per-user cache directories
- owner-only file permissions / ACLs
- atomic writes
- secure deletion
- file locking
- daemon identity verification
- code-signing verification work in the daemon path

That means the distribution format should preserve the same posture instead of weakening it for size wins.

A classic binary packer can reduce download size, but it introduces trade-offs:

- less transparent artifact format
- harder CI and verification story
- higher AV / EDR scrutiny
- weaker control over extraction paths and file permissions
- awkward interaction with your own executable identity checks

The launcher model avoids those problems:

- the real runtime is still a normal signed Windows PE file
- extraction happens into a UFFS-controlled secure directory
- the launcher can reuse your existing `uffs-security` primitives
- the launcher can bind to an exact payload hash and signer identity
- the launcher can support side-by-side versioning and rollback

---

## 3. Goals and non-goals

### 3.1 Goals

1. Reduce the **downloaded artifact size** for Windows releases.
2. Preserve a **single executable user entry point**.
3. Keep the **real payload a normal signed PE file**.
4. Avoid in-memory execution tricks.
5. Make first run safe and deterministic.
6. Make later runs fast.
7. Support versioned payload caching and rollback.
8. Keep CI signing and verification auditable.
9. Fit the current UFFS security posture and coding style.
10. Keep the launcher small, dependency-light, and easy to audit.

### 3.2 Non-goals

1. Do not reduce the final installed footprint on disk. This design reduces distribution size, not expanded size.
2. Do not implement an in-memory PE loader or manual mapper.
3. Do not use self-overwrite of the running launcher as the primary design.
4. Do not rely on obscurity, anti-debugging, or packer-like anti-analysis features.
5. Do not turn the launcher into a full installer in phase 1.
6. Do not make the Windows wrapper the only supported release path forever; MSIX or installer packaging can be added later on top of this.

---

## 4. Core decision

### 4.1 Recommended production architecture

**Use a custom Windows launcher plus a custom packager.**

- **Launcher crate**: tiny runtime stub that validates, extracts, caches, and launches.
- **Packager crate/tool**: release-time tool that compresses the signed payload, generates metadata, and appends the payload blob to the launcher.

### 4.2 Off-the-shelf components to reuse

Reuse mature components for focused tasks instead of reinventing them:

- `zstd` for compression/decompression
- `sha2` for SHA-256 digests
- `windows-sys` (or `windows`) for `CreateProcessW`, `WinVerifyTrust`, file APIs, ACL APIs, mutexes, and handle management
- existing `uffs-security` helpers for secure directory creation, permissions, atomic writes, and file locking
- optional GitHub Actions provenance / SBOM generation
- recommended signing service: Azure Artifact Signing (formerly Trusted Signing) via OIDC in CI

### 4.3 What not to use as the production core

- UPX as the primary release format
- a memory-only loader
- a generic SFX executable with minimal control over trust checks
- self-replacement of the launcher binary after startup

---

## 5. High-level system model

```text
User downloads:  uffs.exe   (this is the launcher / wrapper)

Inside or attached to uffs.exe:
  - baked manifest constants (signed outer launcher)
  - compressed payload blob (signed inner payload after decompression)
  - small trailer/footer that lets the launcher find the blob

First run:
  launcher -> secure extraction root -> verify -> write payload -> atomic rename -> launch payload

Later runs:
  launcher -> detect existing exact payload by hash/version -> quick verify -> launch payload
```

### 5.1 Distribution objects

| Object | Purpose | Signed | Who consumes it |
|---|---|---:|---|
| `uffs-real.exe` | The real UFFS runtime payload | Yes | CI, launcher after extraction |
| `payload.zst` | Compressed bytes of the signed payload | No standalone trust; validated by hash | Packager, launcher |
| `launcher-manifest.json` | Build-time metadata about payload and wrapper | Not trusted by itself; values are baked into signed launcher | Packager |
| `uffs.exe` | User-facing wrapper / launcher | Yes | End user, Windows |
| `%LOCALAPPDATA%\\uffs\\app\\payloads\\<hash>\\uffs-real.exe` | Extracted payload cache | Must match baked hash; optionally verified by Authenticode on install | Launcher |

---

## 6. Trust chain and security model

This is the most important part of the design.

### 6.1 Outer trust

The **launcher** is signed. This provides the Windows trust signal for the file the user downloads and double-clicks.

### 6.2 Inner trust

The **real payload** is also signed before compression. This gives you a normal signed PE file after decompression.

### 6.3 Binding between outer and inner artifacts

The launcher contains baked-in constants that are generated **after** the inner payload is signed:

- inner payload SHA-256
- inner payload size
- payload compressed SHA-256
- payload compressed size
- version
- channel
- target triple
- expected signer identity metadata (optional but recommended)

That means the launcher is bound to **one exact inner binary**.

### 6.4 Why both signatures are needed

You need both:

1. **Outer launcher signature**
   - download trust
   - SmartScreen / reputation / Windows UX alignment
   - user sees a signed file at the actual launch point

2. **Inner payload signature**
   - extracted runtime is still a normal signed PE
   - future daemon / broker / client trust checks can treat it like any other signed executable
   - operational diagnostics remain standard Windows diagnostics

### 6.5 Why the launcher still verifies hashes

Even if both files are signed, the launcher must still verify the inner payload hash because:

- the launcher may store the compressed blob as an appended trailer or other non-standard container region
- Windows PE signatures do not behave like a single flat-file hash over every byte
- the launcher needs deterministic verification without depending on ambiguous packaging details

**Rule:** the launcher trusts the baked-in hash, not the appended payload bytes.

### 6.6 No in-memory execution

The launcher does **not** map the PE into memory and jump to it.

It always:

1. extracts to disk
2. verifies
3. atomically publishes the payload
4. calls `CreateProcessW`

This is important for:

- compatibility with standard Windows process creation
- security tooling visibility
- crash reporting
- trust verification
- not looking like a fileless loader

---

## 7. Compatibility implications for UFFS

This design changes one very important assumption:

> After launch handoff, `current_exe()` in the real UFFS process will point to the extracted payload path, not to the user-visible wrapper path.

That means all code that depends on executable path semantics must be audited.

### 7.1 Things to audit in UFFS

1. `std::env::current_exe()` usage
2. daemon path and executable-hash verification
3. any code that expects sibling files next to the executable
4. self-update logic, if any
5. logging or diagnostics that display executable path
6. broker / daemon startup assumptions on Windows

### 7.2 Recommended rule

Treat the **extracted payload** as the canonical runtime executable.

The wrapper is only a launch bootstrap.

### 7.3 Optional environment variables

The launcher can set:

- `UFFS_WRAPPER_PATH=<path to outer launcher>`
- `UFFS_WRAPPER_VERSION=<wrapper version>`
- `UFFS_PAYLOAD_HASH=<payload sha256>`
- `UFFS_LAUNCHED_VIA_WRAPPER=1`

This helps diagnostics without making the child depend on wrapper location.

---

## 8. Recommended artifact format

There are two viable storage models for the compressed payload:

1. compile it into the launcher PE as a section/resource
2. append it to the launcher as a trailer blob

### 8.1 Recommended model: appended payload blob + baked signed manifest

Use:

- a **small signed launcher PE**
- a **baked manifest** compiled into the launcher before signing
- an **appended compressed blob** and fixed-size footer added after launcher build
- the launcher validates the appended blob against the baked manifest

This gives the best operational trade-off:

- the launcher remains small and quick to compile
- the packager can append the payload without relinking the launcher
- the runtime trust decision is explicit and deterministic
- the launcher does not trust the appended data unless it matches the baked hash

### 8.2 Why not rely on the footer for security

The footer is untrusted metadata. It only tells the launcher where the blob is.

Security comes from the **baked manifest inside the signed launcher**, not from the footer.

### 8.3 Suggested binary layout

```text
[ launcher PE bytes ]
[ compressed payload blob ]
[ fixed-size footer ]
```

### 8.4 Suggested footer layout

```text
Offset from EOF  Size  Field
--------------  ----  ------------------------------------
-40             8     magic = b"UFFSPAY\0"
-32             2     format_version = 1
-30             2     compression_id = 1   ; 1 = zstd
-28             8     payload_len_le
-20             8     inner_len_le
-12             4     reserved
-8              8     footer_crc_or_reserved
```

Notes:

- The footer is intentionally tiny and simple.
- All lengths are sanity-checked against the wrapper file size.
- The footer can include a CRC for corruption detection, but the real security checks are SHA-256 over payload and inner binary.

### 8.5 Suggested baked manifest schema

This JSON is generated by the packager for build/debugging, but the production launcher should consume a generated Rust module or compact binary constant block instead of parsing JSON at runtime.

```json
{
  "format_version": 1,
  "product": "uffs",
  "channel": "stable",
  "target": "x86_64-pc-windows-msvc",
  "wrapper_version": "0.4.38",
  "inner_version": "0.4.38",
  "inner_filename": "uffs-real.exe",
  "inner_sha256": "<sha256 of signed inner PE>",
  "inner_size": 12345678,
  "payload_sha256": "<sha256 of compressed payload blob>",
  "payload_size": 4567890,
  "compression": "zstd",
  "compression_level": 19,
  "expected_publisher_subject": "CN=...",
  "expected_product_name": "UFFS",
  "build_id": "<git sha or release id>",
  "timestamp_utc": "2026-03-31T00:00:00Z"
}
```

### 8.6 Which bytes are hashed

Use these exact rules:

- **Inner hash:** SHA-256 of the fully signed `uffs-real.exe` bytes.
- **Payload hash:** SHA-256 of the compressed payload blob bytes.

Do **not** hash the unsigned inner binary. The launcher must bind to the exact signed runtime that will be extracted.

---

## 9. File system layout

### 9.1 Extraction root

Default root:

```text
%LOCALAPPDATA%\uffs\app\payloads\
```

Use UFFS existing secure directory creation logic or the same ACL rules as the secure cache path.

### 9.2 Versioned / hash-addressed layout

Recommended layout:

```text
%LOCALAPPDATA%\uffs\app\payloads\stable\x86_64\sha256-<INNER_SHA256>\
    uffs-real.exe
    install.meta.json
    install.ok
```

Where:

- `stable` can later become `beta`, `nightly`, etc.
- `x86_64` separates architectures
- the content hash makes the directory immutable by identity

### 9.3 Why hash-addressed directories are better than `current`

Hash-addressed directories give you:

- perfect idempotence
- no ambiguity on upgrade
- trivial rollback
- no need to overwrite a live binary
- easy garbage collection of stale versions

### 9.4 Optional marker files

`install.meta.json` can hold diagnostics only, for example:

- wrapper version used for install
- extracted timestamp
- signature subject
- first-seen timestamp
- last-verified timestamp

`install.ok` is an optional marker written only after successful extraction and verification.

The launcher should never trust the marker alone. The marker only accelerates diagnostics and cleanup.

---

## 10. Runtime state machine

### 10.1 Fast path

1. Launcher starts.
2. It resolves extraction root.
3. It computes the expected target directory from baked constants.
4. If `uffs-real.exe` already exists there:
   - check size
   - check hash
   - optionally check cached verification metadata
5. If valid, launch immediately.

### 10.2 First-run extraction path

1. Launcher starts.
2. No valid extracted payload exists.
3. Acquire extraction lock.
4. Re-check whether another process already installed it.
5. Read footer.
6. Locate compressed payload blob.
7. Verify compressed payload hash.
8. Stream-decompress to a temporary file in the target directory.
9. Flush and close file.
10. Apply secure file permissions / ACL.
11. Verify decompressed file hash.
12. Verify PE signature with `WinVerifyTrust`.
13. Atomically rename temp file to `uffs-real.exe`.
14. Write marker metadata.
15. Launch.

### 10.3 Upgrade path

1. New wrapper ships with new inner hash.
2. New wrapper points to a different target directory.
3. Launcher extracts side-by-side.
4. Old payload remains untouched until cleanup.
5. Rollback is naturally supported by restoring the old wrapper.

### 10.4 Repair path

If the extracted payload exists but fails validation:

- delete or quarantine the broken copy
- re-extract from wrapper
- if re-extraction still fails, abort with a clear error

### 10.5 Concurrent start path

If multiple launcher instances start together:

- one instance acquires the extraction lock
- others wait or retry
- all converge on the same extracted target
- only one installation actually writes files

---

## 11. Detailed launcher algorithm

### 11.1 Startup algorithm

```text
main
  -> locate self path
  -> compute expected install root and final payload path
  -> if valid payload exists:
       launch child
       forward exit code
     else:
       acquire extraction lock
       if valid payload exists after lock:
         launch child
       else:
         extract_and_install
         launch child
```

### 11.2 Validation rules for an already extracted payload

Minimum validation on every run:

1. file exists
2. file size matches baked `inner_size`
3. SHA-256 matches baked `inner_sha256`

Optional validation on install or on policy triggers:

4. `WinVerifyTrust` says the PE signature is valid
5. signer subject matches expected publisher pattern
6. version info matches manifest expectations

### 11.3 Why signature verification can be reduced on the fast path

If the extracted file hash matches the baked hash from the signed launcher, then the extracted bytes are bit-for-bit identical to the exact inner file that CI signed.

That means a full `WinVerifyTrust` verification on every start is usually not necessary. A common policy is:

- **CI:** verify signature every release build
- **launcher install path:** verify signature on first extraction / upgrade
- **launcher steady-state path:** hash-check every launch, full signature check only on repair or policy-driven revalidation

This keeps startup fast while preserving strong integrity.

### 11.4 Extraction algorithm

Pseudo-code:

```rust
fn ensure_payload_installed() -> Result<PathBuf> {
    let root = secure_root()?;
    let final_dir = root.join(channel()).join(arch()).join(hash_dir());
    let final_exe = final_dir.join("uffs-real.exe");

    if valid_existing(&final_exe)? {
        return Ok(final_exe);
    }

    let _lock = acquire_install_lock(&root)?;

    if valid_existing(&final_exe)? {
        return Ok(final_exe);
    }

    create_secure_dir(&final_dir)?;

    let temp_exe = final_dir.join("uffs-real.exe.partial");
    let payload_reader = open_embedded_payload_from_self()?;

    verify_payload_hash(payload_reader.clone())?;
    decompress_to_file(payload_reader, &temp_exe)?;
    flush_file(&temp_exe)?;
    set_owner_only_permissions(&temp_exe)?;

    verify_inner_hash(&temp_exe)?;
    verify_pe_signature(&temp_exe)?;

    atomic_rename(&temp_exe, &final_exe)?;
    write_install_metadata(&final_dir)?;

    Ok(final_exe)
}
```

### 11.5 Launch algorithm

Recommended behavior for CLI and TUI binaries:

- preserve current working directory
- preserve environment
- forward arguments exactly
- wait for child
- return child exit code

For GUI binaries, detaching may be acceptable, but for UFFS CLI / TUI the wrapper should usually forward the child exit code.

### 11.6 Argument forwarding

Be careful here. On Windows, command line quoting rules are tricky.

Recommended implementation choices:

- use `CreateProcessW`
- pass the child executable path explicitly
- reconstruct a child command line from the original arguments using a tested quoting routine
- keep the wrapper out of the child argument list except for diagnostics env vars

### 11.7 Locking model

Use one of:

- a named mutex
- a lock file in the extraction root

Given UFFS already has cross-platform file lock helpers, a lock file under the secure extraction root is reasonable and easy to audit.

Recommended lock path:

```text
%LOCALAPPDATA%\uffs\app\payloads\.install.lock
```

---

## 12. Suggested Rust workspace layout

Add two new crates.

```text
crates/
  uffs-launcher/    # tiny Windows-only runtime wrapper
  uffs-pack/        # build-time packager / appender / manifest generator
```

### 12.1 `uffs-launcher`

Purpose:

- runtime launcher only
- Windows-only target
- very small dependency set

Recommended dependencies:

- `windows-sys` for Win32 APIs
- `sha2`
- `zstd` (or a later pure-Rust alternative if launcher size matters enough)
- `thiserror` or a tiny custom error layer
- optional reuse of small pieces from `uffs-security`

Rules:

- no Tokio
- no Polars
- no Clap unless absolutely required
- no workspace-wide heavy optional dependencies
- no daemon/client logic inside the launcher

### 12.2 `uffs-pack`

Purpose:

- host-side packaging tool used only in CI and release scripts
- generates compressed payload
- generates manifest
- generates baked Rust constants file for the launcher build
- appends payload trailer to built launcher
- computes and prints digests

Recommended dependencies:

- `serde`, `serde_json`
- `sha2`
- `zstd`
- `anyhow`
- minimal filesystem utilities

### 12.3 Shared manifest module

Optionally add:

```text
crates/uffs-launcher-manifest/
```

Only if you want a shared schema between launcher and packager. Otherwise keep the runtime launcher on simple generated constants and let only the packager parse JSON.

### 12.4 Build-time generated file

The packager should emit something like:

```text
crates/uffs-launcher/src/generated/payload_manifest.rs
```

Example generated constants:

```rust
pub const FORMAT_VERSION: u16 = 1;
pub const PRODUCT: &str = "uffs";
pub const CHANNEL: &str = "stable";
pub const TARGET: &str = "x86_64-pc-windows-msvc";
pub const WRAPPER_VERSION: &str = "0.4.38";
pub const INNER_VERSION: &str = "0.4.38";
pub const INNER_FILENAME: &str = "uffs-real.exe";
pub const INNER_SIZE: u64 = 12_345_678;
pub const PAYLOAD_SIZE: u64 = 4_567_890;
pub const INNER_SHA256: [u8; 32] = [ ... ];
pub const PAYLOAD_SHA256: [u8; 32] = [ ... ];
pub const EXPECTED_PUBLISHER_SUBJECT: &str = "CN=...";
```

This keeps the runtime launcher tiny and deterministic.

---

## 13. Build, sign, compress, wrap, sign workflow

This section is the definitive artifact order.

### 13.1 Canonical order

1. Build **inner payload** (`uffs-real.exe`) using the normal UFFS Rust release profile.
2. Sign **inner payload**.
3. Verify **inner payload** signature.
4. Compute **inner payload SHA-256** over the signed bytes.
5. Compress **signed inner payload** to `payload.zst`.
6. Compute **compressed payload SHA-256**.
7. Generate **manifest constants** from the signed inner artifact.
8. Build **launcher stub** using the generated manifest constants.
9. Append compressed payload blob and footer to launcher stub.
10. Sign **outer launcher**.
11. Verify **outer launcher** signature.
12. Smoke-test wrapper behavior on a clean Windows environment.
13. Publish wrapper and optional provenance / SBOM artifacts.

### 13.2 Why this exact order matters

If you compress before signing, the extracted runtime will not be a normal signed PE.

If you build the launcher before the inner payload is signed, the launcher cannot bind to the final signed bytes.

If you sign the launcher before appending the payload, that is acceptable only if the launcher trusts the payload via the baked manifest hash, not by assuming the appended bytes are covered by the outer signature.

---

## 14. Signing architecture

### 14.1 Recommended signing mode for CI

**Preferred:** Azure Artifact Signing with GitHub OIDC.

Why:

- no long-lived private key in GitHub secrets
- Microsoft-managed signing service
- clean audit trail
- works directly in GitHub Actions
- better release engineering hygiene than storing a code-signing cert in the repo or in generic secrets

### 14.2 Important operational note about Artifact Signing

Artifact Signing public-trust availability is currently geographically limited for some account types. If your organization does not fit that availability window, use either:

- Artifact Signing Private Trust, or
- traditional EV / OV code signing with a dedicated secure signer host or HSM-backed process

### 14.3 Timestamping

Always timestamp both inner and outer signatures.

For Artifact Signing, use the Microsoft RFC3161 timestamp endpoint recommended by Microsoft:

```text
http://timestamp.acs.microsoft.com
```

This is critical because Artifact Signing certificates are short-lived and timestamping is what keeps old signed releases verifiable after the certificate validity window ends.

### 14.4 Alternative signing mode

If cloud signing is not available, use:

- a self-hosted Windows signing runner
- SignTool
- EV or OV certificate managed by the organization
- private key stored in HSM, smart card, token, or locked-down cert store

If you go this route, do **not** run the signing job on a generic shared runner.

### 14.5 What to sign

Sign both:

- `uffs-real.exe`
- final `uffs.exe` wrapper

Do **not** skip inner signing just because the launcher is signed.

---

## 15. Suggested CI/CD architecture

### 15.1 Recommended release jobs

Use a dedicated Windows release workflow for Windows wrapper artifacts, even if local developer builds happen from macOS with `cargo-xwin`.

Suggested jobs:

1. `build-inner-windows`
2. `sign-inner`
3. `pack-launcher`
4. `sign-wrapper`
5. `verify-and-smoke-test`
6. `attest-and-publish`

### 15.2 Why release packaging should run on Windows

Even though UFFS developers can cross-build from macOS, the authoritative Windows release path should run on Windows because:

- Artifact Signing GitHub Action runs only on Windows runners
- SignTool and Windows trust verification are native there
- smoke tests should execute the real Windows PE artifacts directly

### 15.3 Runner strategy

Use an explicit Windows runner label, not `windows-latest`, for release workflows.

Recommended starting point:

- `windows-2022` for stability
- test `windows-2025` in a parallel validation workflow before switching

### 15.4 Suggested workflow graph

```text
checkout
  -> build inner payload
  -> sign inner payload
  -> verify inner signature
  -> generate hashes + compress payload
  -> generate launcher manifest constants
  -> build launcher stub
  -> append payload blob
  -> sign wrapper
  -> verify wrapper signature
  -> smoke test wrapper on clean temp profile
  -> create provenance / SBOM / release assets
  -> publish
```

---

## 16. Suggested GitHub Actions workflow skeleton

This is a design skeleton, not a drop-in final file.

```yaml
name: release-windows-wrapper

on:
  workflow_dispatch:
  push:
    tags:
      - 'v*'

permissions:
  contents: read
  id-token: write
  attestations: write

jobs:
  build-package-sign:
    runs-on: windows-2022

    steps:
      - name: Checkout
        uses: actions/checkout@v4

      - name: Setup Rust
        uses: dtolnay/rust-toolchain@stable
        with:
          targets: x86_64-pc-windows-msvc

      - name: Cache Cargo
        uses: Swatinem/rust-cache@v2

      - name: Build inner payload
        shell: pwsh
        run: |
          cargo build --profile dist --target x86_64-pc-windows-msvc -p uffs-cli --bin uffs-real

      - name: Azure login using OIDC
        uses: azure/login@v2
        with:
          client-id: ${{ secrets.AZURE_CLIENT_ID }}
          tenant-id: ${{ secrets.AZURE_TENANT_ID }}
          subscription-id: ${{ secrets.AZURE_SUBSCRIPTION_ID }}

      - name: Sign inner payload
        uses: azure/artifact-signing-action@v1
        with:
          endpoint: https://eus.codesigning.azure.net/
          signing-account-name: ${{ secrets.AZURE_CODE_SIGNING_ACCOUNT }}
          certificate-profile-name: ${{ secrets.AZURE_CODE_SIGNING_PROFILE }}
          files: |
            ${{ github.workspace }}\target\x86_64-pc-windows-msvc\dist\uffs-real.exe
          file-digest: SHA256
          timestamp-rfc3161: http://timestamp.acs.microsoft.com
          timestamp-digest: SHA256

      - name: Verify inner payload signature
        shell: pwsh
        run: |
          signtool verify /pa /all /v target\x86_64-pc-windows-msvc\dist\uffs-real.exe

      - name: Build payload blob and manifest
        shell: pwsh
        run: |
          cargo run -p uffs-pack -- build-payload `
            --input target\x86_64-pc-windows-msvc\dist\uffs-real.exe `
            --out-dir build\payload `
            --product uffs `
            --channel stable `
            --target x86_64-pc-windows-msvc `
            --compression zstd `
            --compression-level 19

      - name: Build launcher stub
        shell: pwsh
        run: |
          cargo build --profile dist --target x86_64-pc-windows-msvc -p uffs-launcher --bin uffs

      - name: Append payload to launcher
        shell: pwsh
        run: |
          cargo run -p uffs-pack -- append-payload `
            --launcher target\x86_64-pc-windows-msvc\dist\uffs.exe `
            --payload build\payload\payload.zst `
            --manifest build\payload\launcher-manifest.json `
            --output dist\uffs.exe

      - name: Sign outer wrapper
        uses: azure/artifact-signing-action@v1
        with:
          endpoint: https://eus.codesigning.azure.net/
          signing-account-name: ${{ secrets.AZURE_CODE_SIGNING_ACCOUNT }}
          certificate-profile-name: ${{ secrets.AZURE_CODE_SIGNING_PROFILE }}
          files: |
            ${{ github.workspace }}\dist\uffs.exe
          file-digest: SHA256
          timestamp-rfc3161: http://timestamp.acs.microsoft.com
          timestamp-digest: SHA256

      - name: Verify outer wrapper signature
        shell: pwsh
        run: |
          signtool verify /pa /all /v dist\uffs.exe

      - name: Smoke test wrapper first and second run
        shell: pwsh
        run: |
          $testRoot = Join-Path $env:RUNNER_TEMP 'uffs-launcher-smoke'
          Remove-Item -Recurse -Force $testRoot -ErrorAction SilentlyContinue
          New-Item -ItemType Directory -Force -Path $testRoot | Out-Null
          $env:LOCALAPPDATA = $testRoot
          .\dist\uffs.exe --version
          .\dist\uffs.exe --version

      - name: Generate provenance attestation
        uses: actions/attest@v4
        with:
          subject-path: dist\uffs.exe

      - name: Upload artifacts
        uses: actions/upload-artifact@v4
        with:
          name: uffs-windows-wrapper
          path: |
            dist\uffs.exe
            build\payload\launcher-manifest.json
```

### 16.1 Notes on the workflow

- Build and signing happen on Windows.
- Inner and outer artifacts are signed separately.
- The packaging step happens between the two signing steps.
- Smoke test runs the final signed wrapper twice to exercise both extraction and fast path.
- Provenance attestation is generated over the final release artifact.

### 16.2 Optional follow-up jobs

You can add:

- VirusTotal / Defender preflight submission checks
- additional clean-VM smoke tests
- SBOM generation and attestation
- release upload to GitHub Releases / website / package manager feed

---

## 16A. Integration with the current UFFS release toolchain

UFFS already has:

- a workspace with multiple binaries
- `cargo-dist` metadata
- release automation
- macOS-to-Windows cross-build habits via `cargo-xwin`

The launcher architecture should fit that reality instead of forcing a total release-process rewrite.

### 16A.1 Binary naming strategy

Recommended naming for the Windows wrapper flow:

- internal real payload binary: `uffs-real.exe`
- public launcher binary: `uffs.exe`

That keeps the user-visible entry point unchanged while making the build pipeline explicit.

### 16A.2 Cargo targets and package responsibilities

Recommended package split:

- `uffs-cli` continues to own the real product logic
- `uffs-launcher` owns only the wrapper logic
- `uffs-pack` owns only release-time packaging

If you later add a wrapped TUI artifact, use the same pattern:

- `uffs-tui-real.exe` as the internal payload
- `uffs_tui.exe` or `uffs-tui.exe` as the public wrapper

### 16A.3 How to use `cargo-dist`

Recommended phase-1 approach:

- keep `cargo-dist` for raw build orchestration and non-Windows assets
- treat the Windows wrapper flow as a **post-build packaging stage**
- publish the final signed wrapper as the real Windows release artifact

In practical terms:

1. `cargo-dist` or a normal Cargo build produces the unsigned inner Windows artifact.
2. the Windows release workflow signs the inner artifact
3. the workflow packages the wrapper
4. the workflow signs the wrapper
5. the workflow uploads the wrapper as the Windows asset users download

This is usually simpler than trying to teach `cargo-dist` about a two-level signing pipeline on day one.

### 16A.4 Suggested internal release artifact names

```text
artifacts/
  windows-x64/
    uffs-real-unsigned.exe
    uffs-real-signed.exe
    payload.zst
    launcher-manifest.json
    uffs-launcher-unsigned.exe
    uffs.exe                 # final signed wrapper
    uffs.exe.sha256
    uffs.exe.provenance.jsonl
    uffs.spdx.json
```

### 16A.5 Suggested release-script changes

Your current quick deploy flow can evolve into two explicit paths:

1. `quick-deploy-inner`
   - build raw Windows payload only
   - useful for debugging

2. `quick-deploy-wrapper`
   - build inner payload
   - dev-sign or skip signing
   - compress
   - build launcher
   - append payload
   - produce a local wrapper for smoke testing

3. `release-windows-wrapper`
   - CI-only authoritative pipeline
   - official signing
   - final verification
   - publication

This keeps local iteration fast while keeping the production release path locked down.

### 16A.6 Cross-build vs release-build rule

Recommended policy:

- developers may cross-build the **inner payload** from macOS using `cargo-xwin`
- developers may locally package a wrapper for testing
- only the Windows CI release pipeline may produce the **official signed wrapper**

That rule avoids signature drift, environment drift, and support confusion.

## 17. Recommended local developer workflow

### 17.1 Developer inner-build workflow on macOS

Keep your current local cross-build loop:

```bash
cargo xwin build --profile dist --target x86_64-pc-windows-msvc -p uffs-cli --bin uffs-real
```

This is fine for dev iteration.

### 17.2 Optional local wrapper packaging (unsigned or dev-signed)

Developers can optionally run:

```bash
cargo run -p uffs-pack -- build-payload --input ... --out-dir ...
cargo build --profile dist --target x86_64-pc-windows-msvc -p uffs-launcher --bin uffs
cargo run -p uffs-pack -- append-payload --launcher ... --payload ... --output ...
```

This produces a functional dev wrapper, but it is **not** the authoritative release artifact.

### 17.3 Authoritative release rule

The release artifact that users download should come only from the Windows CI release pipeline after official signing and verification.

---

## 18. Implementation detail: launcher startup semantics

### 18.1 Parent / child lifecycle

Recommended default for UFFS CLI and TUI:

- launcher waits for child process to exit
- launcher returns the same exit code
- stdout / stderr remain attached to the same console

This makes the wrapper effectively invisible to users and scripts.

### 18.2 Working directory

Preserve the caller's working directory.

### 18.3 Environment

Forward environment as-is, with only a few optional diagnostic variables added.

### 18.4 Elevation and UAC

The wrapper should not introduce elevation requirements if the inner payload does not need them.

Extraction to `%LOCALAPPDATA%` avoids requiring admin rights for normal use.

### 18.5 File association / shell behavior

The user-facing executable remains the wrapper path, so shortcuts and user habits remain stable.

---

## 19. How to keep execution fast

### 19.1 Use compression for distribution, not for every launch

The whole design depends on this rule:

- **decompress once per version/hash**
- **launch the extracted binary directly thereafter**

### 19.2 Compression choice

Recommended default: **Zstandard**.

Why:

- strong compression
- much faster decompression than xz / lzma for first-run install
- already aligned with your Rust stack

### 19.3 Compression level

Recommended starting point:

- release packaging: `zstd` level `19`
- benchmark `15`, `19`, and `22`

Higher levels may win a little on download size but cost more in CI time.

### 19.4 Do not decompress into memory first

Stream directly from the wrapper payload reader into the output file.

Benefits:

- lower peak memory
- simpler behavior for large payloads
- better crash tolerance

---

## 20. Recommended launcher error handling and UX

The launcher should be quiet on success and explicit on failure.

### 20.1 Recommended failure classes

1. cannot resolve secure extraction directory
2. cannot acquire install lock
3. payload footer invalid
4. compressed payload hash mismatch
5. decompression failed
6. extracted payload hash mismatch
7. extracted payload signature invalid
8. process launch failed

### 20.2 User-facing behavior

For CLI/TUI, print a concise error to stderr such as:

```text
UFFS launcher error: extracted runtime failed integrity verification.
Please re-download the application.
```

Log more detail to tracing or diagnostic files if available.

### 20.3 Never silently run an unverified payload

If verification fails, abort. Do not fall back to "best effort" execution.

---

## 21. Cleanup and retention strategy

### 21.1 Keep current version, retain one previous version

Recommended policy:

- keep active hash directory
- keep most recent previous hash directory
- delete older ones during idle cleanup or next successful upgrade

### 21.2 Cleanup timing

Do not delete old payloads before a successful new launch.

Preferred strategy:

- extract new payload
- launch successfully
- schedule cleanup on next start or explicit maintenance path

### 21.3 Shared lock during cleanup

Cleanup should reuse the same root lock so you never race extraction and deletion.

---

## 22. Suggested test matrix

### 22.1 Functional tests

1. first run extracts and launches
2. second run launches without re-extracting
3. deleting extracted payload causes clean re-extraction
4. tampering with extracted payload causes repair or hard failure
5. tampering with appended compressed blob causes hard failure
6. concurrent starts do not corrupt installation
7. wrapper forwards arguments correctly
8. wrapper forwards exit code correctly

### 22.2 Security tests

1. outer wrapper signature verifies in CI
2. inner payload signature verifies in CI
3. launcher rejects wrong inner hash
4. launcher rejects wrong payload hash
5. launcher uses secure directory permissions
6. extracted payload path is outside temp directories
7. no in-memory execution path exists

### 22.3 Crash / interruption tests

1. crash during extraction leaves no published corrupt runtime
2. power-loss simulation during install keeps old version intact
3. stale `.partial` files are cleaned safely

### 22.4 Compatibility tests

1. CLI works when launched from PowerShell, cmd.exe, Explorer, and scripts
2. TUI works with console inheritance
3. daemon identity logic works from extracted payload path
4. wrapper behaves correctly when moved to a different directory

### 22.5 Windows trust tests

1. `signtool verify /pa /all` passes for wrapper
2. `signtool verify /pa /all` passes for extracted inner payload
3. `Get-AuthenticodeSignature` shows expected subject/publisher
4. Defender / SmartScreen sanity test on a clean machine before broad release

---

## 23. Rollout plan

### Phase 0: benchmark and prototype

- build one custom launcher prototype
- measure wrapper size reduction vs raw inner EXE
- measure first-run extraction time
- measure second-run overhead
- smoke test with Defender on clean Windows VMs

### Phase 1: internal release path

- wire CI build -> sign inner -> pack -> sign wrapper
- publish only to internal users
- collect launch timings and trust tool feedback

### Phase 2: limited public release

- ship wrapper to a small subset of Windows users
- validate SmartScreen / AV behavior
- verify supportability and crash reporting

### Phase 3: general availability

- make wrapper the default Windows portable download
- optionally layer NSIS / installer / MSIX on top later

---

## 24. Alternatives considered

### 24.1 UPX / PE packers

Pros:

- simple
- one-file output
- often strong size reduction

Cons:

- weaker operational transparency
- more likely to attract scrutiny from security tools
- awkward fit with your explicit executable identity model
- less control over secure extraction and validation behavior

Decision: not the primary production path.

### 24.2 In-memory PE loader

Pros:

- avoids extracted file on disk

Cons:

- much riskier
- more likely to look suspicious to security tooling
- harder crash/debug/tooling compatibility
- incompatible with the goal of preserving a standard signed PE runtime

Decision: reject.

### 24.3 Self-replacing launcher

Pros:

- one file appears to "become" the full program

Cons:

- ugly Windows file replacement semantics
- more operational complexity
- not needed when hash-addressed side-by-side extraction already solves versioning cleanly

Decision: reject as the primary design.

### 24.4 Generic SFX installer

Pros:

- mature tooling
- smaller download

Cons:

- less precise control over UFFS trust checks and extraction policy
- tends to drift into installer behavior rather than transparent launcher behavior

Decision: optional secondary packaging layer, not the core runtime model.

---

## 25. Concrete recommendations for UFFS

1. Add `crates/uffs-launcher`.
2. Add `crates/uffs-pack`.
3. Keep the launcher Windows-only and dependency-light.
4. Keep the real app binary separate and fully signed before compression.
5. Use `%LOCALAPPDATA%\\uffs\\app\\payloads\\<channel>\\<arch>\\sha256-<hash>\\uffs-real.exe` as the runtime path.
6. Reuse `uffs-security` for secure directories, permissions, atomic writes, and locking.
7. Build release wrappers on Windows CI, not on macOS release hosts.
8. Prefer Azure Artifact Signing with OIDC for CI signing.
9. Sign both inner payload and outer wrapper.
10. Hash-check on every launch; full PE signature verify on install / repair / CI.
11. Treat the extracted payload path as the canonical runtime executable path.
12. Do not use UPX, memory loaders, or self-overwrite for production.

---

## 26. Minimum viable implementation plan

### 26.1 Week 1

- create `uffs-launcher`
- create `uffs-pack`
- implement manifest generation
- implement footer appending and reading
- implement secure extraction to versioned hash path
- implement `CreateProcessW` launch and exit code forwarding

### 26.2 Week 2

- add inner payload signing and verification in CI
- add outer wrapper signing and verification in CI
- add smoke tests for first-run and second-run paths
- add cleanup logic and concurrent start protection

### 26.3 Week 3

- audit `current_exe()` assumptions in UFFS
- audit daemon identity / broker path expectations
- add build provenance and SBOM generation
- run Defender / SmartScreen preflight testing

---

## 27. Final recommendation

For UFFS, the right long-term architecture is:

> A custom signed Windows launcher that carries a compressed copy of the separately signed real UFFS runtime, extracts it into a secure per-user hash-addressed directory on first use, verifies it, and launches it directly on later runs.

This gives you:

- smaller distribution artifacts
- fast steady-state execution
- normal signed PE payloads after extraction
- compatibility with your current security posture
- clean CI signing and verification
- side-by-side versioning and rollback
- no reliance on suspicious memory-loader behavior

This is the architecture I would implement.

---

## 28. Reference notes for implementation and CI

These are the external references that informed the architecture and the CI guidance.

1. Microsoft Learn - Artifact Signing quickstart
   - https://learn.microsoft.com/en-us/azure/artifact-signing/quickstart
2. Microsoft Learn - Artifact Signing integrations and SignTool requirements
   - https://learn.microsoft.com/en-us/azure/artifact-signing/how-to-signing-integrations
3. GitHub - Azure Artifact Signing Action
   - https://github.com/Azure/artifact-signing-action
4. Microsoft Learn - Authenticate to Azure from GitHub Actions using OIDC
   - https://learn.microsoft.com/en-us/azure/developer/github/connect-from-azure-openid-connect
5. Microsoft Learn - SignTool overview
   - https://learn.microsoft.com/en-us/windows/win32/seccrypto/signtool
6. Microsoft Learn - Use SignTool to sign a file
   - https://learn.microsoft.com/en-us/windows/win32/seccrypto/using-signtool-to-sign-a-file
7. Microsoft Learn - WinVerifyTrust PE verification example
   - https://learn.microsoft.com/en-us/windows/win32/seccrypto/example-c-program--verifying-the-signature-of-a-pe-file
8. Microsoft Learn - Understanding executable file signing
   - https://learn.microsoft.com/en-us/windows/win32/secbp/understanding-pe-signatures
9. Microsoft Learn - CreateProcess documentation
   - https://learn.microsoft.com/en-us/windows/win32/procthread/creating-processes
10. GitHub Docs - Artifact attestations
    - https://docs.github.com/en/actions/concepts/security/artifact-attestations
11. GitHub Docs - Using artifact attestations
    - https://docs.github.com/en/actions/how-tos/secure-your-work/use-artifact-attestations
12. GitHub - actions/attest
    - https://github.com/actions/attest
13. GitHub - wrappe (reference only, not the final production recommendation)
    - https://github.com/Systemcluster/wrappe
14. GitHub - rc-zip trailing ZIP support (reference only)
    - https://github.com/bearcove/rc-zip
15. Microsoft Defender SmartScreen overview
    - https://learn.microsoft.com/en-us/windows/security/operating-system-security/virus-and-threat-protection/microsoft-defender-smartscreen/
16. Microsoft Edge SmartScreen reputation notes
    - https://learn.microsoft.com/en-us/deployedge/microsoft-edge-security-smartscreen
17. MITRE ATT&CK - Software Packing
    - https://attack.mitre.org/techniques/T1027/002/

---

## 29. Optional appendix: future extensions

1. Machine-wide install mode using `%ProgramFiles%` plus a service/installer
2. MSIX packaging on top of the same inner payload
3. differential payload updates between versions
4. shared extracted runtime for multiple UFFS entry points
5. launcher telemetry for extraction duration and repair events
6. signed manifest catalogs if you later need third-party wrapper generation

