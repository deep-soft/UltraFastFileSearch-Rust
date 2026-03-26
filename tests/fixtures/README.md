# Test Fixtures

Binary test data files for integration testing.

## drive_g/ — Small USB Drive (~20K records)

| File | Size | Description |
|------|------|-------------|
| `G_mft.bin` | 20 MB | Raw MFT dump from a small USB drive (drive G:) |
| `G_mft.iocp` | 672 KB | IOCP-captured MFT (compressed format) |

**Note**: We use the raw MFT file directly — not `.uffs` cache files.
The `.uffs` cache is encrypted (S2 AES-GCM) and tied to the machine's
keychain, so it's not portable. The daemon parses the raw MFT on the fly
using `--mft-file` + `--no-cache`.

### Usage in Tests

```rust
let fixture_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
    .parent().unwrap()  // crates/
    .parent().unwrap()  // repo root
    .join("tests/fixtures/drive_g");
let mft_path = fixture_dir.join("G_mft.bin");
```

### Data Content

Real MFT from a small USB flash drive:
- ~20,000 file/directory records
- Standard Windows directory structure
- No sensitive personal data (just filenames and metadata)
