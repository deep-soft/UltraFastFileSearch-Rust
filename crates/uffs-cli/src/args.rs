//! CLI argument definitions: `Cli` struct and `Commands` enum.

use std::path::PathBuf;

use clap::{Parser, Subcommand};

/// Parse a drive letter from common CLI input formats.
///
/// Accepts:
/// - Single letter: `C`, `c`
/// - With colon: `C:`, `c:`
///
/// Returns uppercase drive letter.
pub fn parse_drive_letter(input: &str) -> Result<char, String> {
    let trimmed = input.trim();
    // Strip trailing colon if present (`C:` -> `C`).
    let letter_str = trimmed.strip_suffix(':').unwrap_or(trimmed);

    if letter_str.len() != 1 {
        return Err(format!(
            "Invalid drive letter '{input}': expected single letter like 'C' or 'C:'"
        ));
    }

    let ch = letter_str
        .chars()
        .next()
        .ok_or_else(|| format!("Invalid drive letter '{input}'"))?;

    if !ch.is_ascii_alphabetic() {
        return Err(format!("Invalid drive letter '{input}': must be A-Z"));
    }

    Ok(ch.to_ascii_uppercase())
}

/// Parse a human-readable size string for clap `value_parser`.
///
/// Delegates to [`uffs_core::search::filters::parse_size`] which accepts
/// bare integers (bytes) and suffixes: `B`, `KB`, `MB`, `GB`, `TB`.
fn parse_size_arg(input: &str) -> Result<u64, String> {
    uffs_core::search::filters::parse_size(input)
}

/// UFFS - Ultra Fast File Search using direct MFT reading
#[derive(Parser)]
#[command(name = "uffs")]
#[command(
    author,
    version,
    about = "Command-line interface for UFFS (Ultra Fast File Search)",
    long_about = "Fast NTFS search via direct Master File Table reads.\n\nSearch is the default action: pass a pattern with no subcommand to search a live volume, a saved index, or a raw MFT file. Use subcommands for index creation and offline inspection.",
    after_help = "Examples:\n  uffs '*.txt'\n  uffs '>.*\\.log$' --drive C\n  uffs '*' --mft-file G_mft.bin --drive G\n  uffs index -d C index.parquet"
)]
#[command(propagate_version = true)]
#[command(args_conflicts_with_subcommands = true)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "CLI args struct mirrors many boolean flags from clap"
)]
pub struct Cli {
    /// Enable verbose output
    #[arg(short, long, global = true)]
    pub verbose: bool,

    /// Subcommand to execute (search, index, info, stats, save-raw, load-raw).
    #[command(subcommand)]
    pub command: Option<Commands>,

    /// Search pattern (glob, regex with `>`, or literal) - default action
    ///
    /// When no subcommand is specified, uffs performs a search.
    /// Examples:
    ///   `uffs *.txt`           - All .txt files
    ///   `uffs c:/pro*`         - Files starting with "pro" on C:
    ///   `uffs ">.*\.log$"`     - REGEX for .log files
    #[arg(value_name = "PATTERN", verbatim_doc_comment)]
    pub pattern: Option<String>,

    /// Drive letter to search (e.g., C or C:). Overrides drive in pattern.
    #[arg(short, long, conflicts_with = "drives", value_parser = parse_drive_letter)]
    pub drive: Option<char>,

    /// Multiple drive letters to search concurrently (e.g., C,D,E or C:,D:,E:)
    #[arg(long, value_delimiter = ',', conflicts_with = "drive", value_parser = parse_drive_letter)]
    pub drives: Option<Vec<char>>,

    /// Use raw MFT file(s) instead of live MFT (cross-platform)
    ///
    /// Load previously saved raw MFT files (from `uffs save-raw` or
    /// `uffs_mft save`). Drive letters are auto-inferred from filenames
    /// (e.g., `C.bin` → C:, `D_mft.bin` → D:). Use `--drive`/`--drives` to
    /// override if needed.
    ///   Single:  `uffs "*" --mft-file C.bin`
    ///   Multi:   `uffs "*" --mft-file C.bin,D.bin`
    #[arg(long, value_delimiter = ',', verbatim_doc_comment)]
    pub mft_file: Vec<PathBuf>,

    /// Data directory containing `drive_*` subdirectories with MFT files
    ///
    /// Auto-discovers all MFT files in `drive_c/`, `drive_d/`, etc.
    /// within the given directory. Prefers `.iocp` over `.bin` over `.mft`.
    ///   Example: `uffs "*" --data-dir ~/uffs_data`
    #[arg(long, verbatim_doc_comment)]
    pub data_dir: Option<PathBuf>,

    /// Show only files (exclude directories)
    #[arg(long)]
    pub files_only: bool,

    /// Show only directories
    #[arg(long)]
    pub dirs_only: bool,

    /// Hide system files (files starting with $)
    #[arg(long)]
    pub hide_system: bool,

    /// Hide NTFS Alternate Data Streams from results
    #[arg(long)]
    pub hide_ads: bool,

    /// Show detailed timing breakdown for performance profiling
    #[arg(long)]
    pub profile: bool,

    /// Run aggregate analytics alongside search results.
    ///
    /// Pass one or more aggregate specs using power syntax.
    /// Examples: --agg count  --agg "terms:extension,top=50"
    ///           --agg "stats:size"  --agg "preset:overview"
    #[arg(long = "agg", value_name = "SPEC")]
    pub agg: Vec<String>,

    /// Show only the total matching count (no rows).
    ///
    /// Shorthand for `--agg count`. Suppresses row output.
    #[arg(long)]
    pub count: bool,

    /// Show a facet breakdown by field (no rows).
    ///
    /// Accepts `FIELD` or `FIELD:TOP` (default top=20).
    /// Shorthand for `--agg "terms:FIELD,top=TOP"`.
    /// Examples: `--facet extension`  `--facet type:10`
    #[arg(long, value_name = "FIELD[:TOP]")]
    pub facet: Vec<String>,

    /// Show scalar statistics for a numeric field (no rows).
    ///
    /// Shorthand for `--agg "stats:FIELD"`.
    /// Examples: `--stats size`  `--stats allocated_size`
    #[arg(long, value_name = "FIELD")]
    pub stats: Vec<String>,

    /// Show a histogram for a field (no rows).
    ///
    /// Accepts `FIELD` or `FIELD:INTERVAL`.
    /// Shorthand for `--agg "hist:FIELD,interval=INTERVAL"`.
    /// Examples: `--histogram size`  `--histogram size:1048576`
    #[arg(long, value_name = "FIELD[:INTERVAL]")]
    pub histogram: Vec<String>,

    /// Include matching rows alongside aggregate results.
    ///
    /// By default, aggregate flags (`--count`, `--facet`, `--stats`,
    /// `--histogram`, `--agg`) suppress row output. Use `--rows` to
    /// get both rows and aggregates.
    #[arg(long)]
    pub rows: bool,

    /// Debug tree metrics computation (prints detailed hardlink handling info)
    #[arg(long, hide = true)]
    pub debug_tree: bool,

    /// Benchmark mode: skip output, only measure MFT reading and filtering
    /// Use this for profiling without stdout I/O overhead
    #[arg(long)]
    pub benchmark: bool,

    /// Disable MFT bitmap optimization (read ALL records)
    /// Use this for debugging if records appear to be missing
    #[arg(long)]
    pub no_bitmap: bool,

    /// Bypass cache and read MFT fresh (default: use cache)
    #[arg(long)]
    pub no_cache: bool,

    /// Minimum file size (e.g. 100KB, 10MB, 1GB, or raw bytes)
    #[arg(long, value_parser = parse_size_arg)]
    pub min_size: Option<u64>,

    /// Maximum file size (e.g. 100KB, 10MB, 1GB, or raw bytes)
    #[arg(long, value_parser = parse_size_arg)]
    pub max_size: Option<u64>,

    /// Minimum descendant count (directories only)
    ///
    /// Filter directories by minimum number of child entries.
    /// Example: --min-descendants 10 (dirs with at least 10 children)
    #[arg(long)]
    pub min_descendants: Option<u32>,

    /// Maximum descendant count (directories only)
    ///
    /// Filter directories by maximum number of child entries.
    /// Example: --max-descendants 0 (empty directories)
    #[arg(long)]
    pub max_descendants: Option<u32>,

    /// Exact descendant count (shortcut for --min-descendants N
    /// --max-descendants N)
    #[arg(long)]
    pub exact_descendants: Option<u32>,

    /// Minimum filename length in characters
    ///
    /// Example: --min-name-length 20
    #[arg(long)]
    pub min_name_length: Option<u16>,

    /// Maximum filename length in characters
    ///
    /// Example: --max-name-length 10
    #[arg(long)]
    pub max_name_length: Option<u16>,

    /// Minimum full-path length in characters
    ///
    /// Example: --min-path-length 100
    #[arg(long)]
    pub min_path_length: Option<u16>,

    /// Maximum full-path length in characters (useful for `MAX_PATH` detection)
    ///
    /// Example: --max-path-length 260
    #[arg(long)]
    pub max_path_length: Option<u16>,

    /// Minimum on-disk (allocated) size (e.g. 100KB, 10MB, 1GB)
    #[arg(long, value_parser = parse_size_arg)]
    pub min_size_on_disk: Option<u64>,

    /// Maximum on-disk (allocated) size (e.g. 100KB, 10MB, 1GB)
    #[arg(long, value_parser = parse_size_arg)]
    pub max_size_on_disk: Option<u64>,

    /// Exact file size (shortcut for --min-size N --max-size N)
    #[arg(long, value_parser = parse_size_arg)]
    pub exact_size: Option<u64>,

    /// Exact on-disk size (shortcut for --min-size-on-disk N --max-size-on-disk
    /// N)
    #[arg(long, value_parser = parse_size_arg)]
    pub exact_size_on_disk: Option<u64>,

    /// Minimum subtree logical size (e.g. 1GB — directories with at least 1GB
    /// of files)
    #[arg(long, value_parser = parse_size_arg)]
    pub min_treesize: Option<u64>,

    /// Maximum subtree logical size
    #[arg(long, value_parser = parse_size_arg)]
    pub max_treesize: Option<u64>,

    /// Minimum subtree on-disk size (e.g. 10GB — directories using at least
    /// 10GB on disk)
    #[arg(long, value_parser = parse_size_arg)]
    pub min_tree_allocated: Option<u64>,

    /// Maximum subtree on-disk size
    #[arg(long, value_parser = parse_size_arg)]
    pub max_tree_allocated: Option<u64>,

    /// Filter by month-of-year or quarter (applied to modified time)
    ///
    /// Accepts month names (january, jan), quarters (Q1..Q4), or
    /// comma-separated combos (jan,feb or Q1,Q3).
    /// Matches files from ANY year in the given months.
    #[arg(long)]
    pub month: Option<String>,

    /// Time range shorthand: --between START,END
    ///
    /// Equivalent to --newer START --older END. Accepts same time specs.
    /// Example: --between 2026-01-01,2026-03-31
    #[arg(long)]
    pub between: Option<String>,

    /// Maximum number of results (0 = unlimited)
    #[arg(short = 'n', long, default_value = "0")]
    pub limit: u32,

    /// Output format: table, json, csv, custom
    #[arg(short, long, default_value = "csv")]
    pub format: String,

    /// Case-sensitive matching (default: off)
    #[arg(long, default_value = "false")]
    pub case: bool,

    /// Smart case: auto case-sensitive if pattern contains uppercase (default:
    /// off)
    ///
    /// When enabled, patterns with ANY uppercase letter become case-sensitive
    /// automatically. Lowercase-only patterns stay case-insensitive.
    /// Like `fd --smart-case` / `ripgrep --smart-case`.
    #[arg(long, default_value = "false")]
    pub smart_case: bool,

    /// Filter by NTFS attributes (comma-separated, prefix ! to exclude)
    ///
    /// Examples: hidden, !hidden, compressed,encrypted, !system,!hidden
    /// Available: hidden, system, archive, readonly, compressed, encrypted,
    ///   sparse, reparse, offline, notindexed, temporary, virtual,
    ///   pinned, unpinned, integrity, noscrub, directory
    #[arg(long)]
    pub attr: Option<String>,

    /// Only files modified within this duration/after this date
    ///
    /// Examples: 7d (7 days), 24h (24 hours), 30m (30 minutes),
    ///   2026-01-15, 2026-01-15T10:30:00
    #[arg(long)]
    pub newer: Option<String>,

    /// Only files modified before this duration/date
    #[arg(long)]
    pub older: Option<String>,

    /// Only files created within this duration/after this date
    #[arg(long)]
    pub newer_created: Option<String>,

    /// Only files created before this duration/date
    #[arg(long)]
    pub older_created: Option<String>,

    /// Only files accessed within this duration/after this date
    #[arg(long)]
    pub newer_accessed: Option<String>,

    /// Only files accessed before this duration/date
    #[arg(long)]
    pub older_accessed: Option<String>,

    /// Exclude files matching this pattern (applied after main pattern)
    ///
    /// Example: uffs *.txt --exclude backup*
    #[arg(long)]
    pub exclude: Option<String>,

    /// Filter by directory path (glob pattern matched against directory portion
    /// only)
    ///
    /// Example: uffs *.rs --in-path projects
    /// Example: uffs *.log --in-path *temp*
    #[arg(long)]
    pub in_path: Option<String>,

    /// Filter by file type/category
    ///
    /// Categories: archive, audio, backup, cad, cert, code, config,
    ///   data, database, directory, disk, document, ebook, executable,
    ///   file, font, log, other, picture, script, shortcut, system, video, web
    ///
    /// Example: uffs "*" --type code
    /// Example: uffs "*" --type picture
    #[arg(long = "type", value_name = "CATEGORY")]
    pub type_filter: Option<String>,

    /// Minimum bulkiness — allocated-to-size ratio as percentage
    ///
    /// 100 = perfectly packed, 200 = 2× wasted space.
    /// Example: uffs "*" --min-bulkiness 500  (files wasting ≥5× their size)
    #[arg(long)]
    pub min_bulkiness: Option<u64>,

    /// Maximum bulkiness
    #[arg(long)]
    pub max_bulkiness: Option<u64>,

    /// Match files whose name begins with PREFIX (sugar for 'PREFIX*')
    ///
    /// Example: uffs --begins-with report
    #[arg(long, conflicts_with = "pattern")]
    pub begins_with: Option<String>,

    /// Match files whose name ends with SUFFIX (sugar for '*SUFFIX')
    ///
    /// Example: uffs --ends-with _backup
    #[arg(long, conflicts_with = "pattern")]
    pub ends_with: Option<String>,

    /// Match files whose name contains NEEDLE (sugar for '*NEEDLE*')
    ///
    /// Example: uffs --contains invoice
    #[arg(long, conflicts_with = "pattern")]
    pub contains: Option<String>,

    /// Exclude files whose name contains NEEDLE (sugar for --exclude
    /// '*NEEDLE*')
    ///
    /// Example: uffs *.log --not-contains debug
    #[arg(long)]
    pub not_contains: Option<String>,

    /// Whole word matching (wraps pattern in \b...\b regex)
    ///
    /// Example: uffs --word nice  (finds "nice" but not "nicehouse")
    #[arg(long, default_value = "false")]
    pub word: bool,

    /// Match against filename only, not the full path
    ///
    /// By default, literal patterns like "hallo" match anywhere in the full
    /// path (including directory names). With --name-only, matching is
    /// restricted to the filename component — like C++ UFFS behavior.
    ///
    /// Incompatible with patterns containing path separators (\ or /).
    #[arg(long, default_value = "false")]
    pub name_only: bool,

    /// Sort results by column(s), comma-separated for multi-tier
    ///
    /// Prefix with `-` for descending, e.g. `-size` or `-modified,name`.
    /// Use `:asc`/`:desc` suffix for explicit direction, e.g. `size:asc`.
    /// Without direction, `--sort X` uses ascending; `--sort-desc` flips it.
    ///
    /// Examples: size, modified, name, -size,name, modified:desc,size:asc
    /// Available: size, sizeondisk, modified, created, accessed, name,
    ///   ext, descendants, hidden, system, archive, readonly,
    ///   compressed, encrypted, directory
    #[arg(long, allow_hyphen_values = true)]
    pub sort: Option<String>,

    /// Reverse sort order (descending)
    #[arg(long, default_value = "false")]
    pub sort_desc: bool,

    /// Filter by file extension(s)
    #[arg(long)]
    pub ext: Option<String>,

    /// Output destination: console or filename
    #[arg(long, default_value = "console")]
    pub out: String,

    /// Columns to output (comma-separated or "all")
    /// Default: all columns.
    #[arg(long, default_value = "all")]
    pub columns: String,

    /// Column separator (default: comma)
    #[arg(long, default_value = ",")]
    pub sep: String,

    /// Quote character for string values (default: double quote)
    #[arg(long, default_value = "\"")]
    pub quotes: String,

    /// Include header row in output (--header=false or --header false to
    /// suppress).
    #[arg(long, default_value_t = true, action = clap::ArgAction::Set)]
    pub header: bool,

    /// Representation for active/true boolean attributes
    #[arg(long, default_value = "1")]
    pub pos: String,

    /// Representation for inactive/false boolean attributes
    #[arg(long, default_value = "0")]
    pub neg: String,

    /// Query execution mode: auto, index, dataframe
    ///
    /// - auto: Automatically choose best path (default)
    /// - index: Force fast `MftIndex` path (simple queries only)
    /// - dataframe: Force Polars `DataFrame` path (full features)
    #[arg(long, default_value = "auto", verbatim_doc_comment)]
    pub query_mode: String,

    /// Override timezone offset for timestamp display (hours from UTC).
    ///
    /// By default, timestamps are displayed in the current local timezone.
    /// Use this to force a specific offset, e.g. for reproducible parity
    /// testing when the reference was generated in a different DST period.
    ///
    /// Examples: -8 (PST), -7 (PDT), 0 (UTC), 1 (CET), 9 (JST)
    #[arg(long, allow_hyphen_values = true)]
    pub tz_offset: Option<i32>,

    /// Chaos mode seed for testing (randomizes chunk order).
    ///
    /// Only works with `--mft-file`. Reads MFT chunks in pseudo-random order
    /// to verify that directory index merging works correctly regardless of
    /// read order. Used for regression testing.
    #[arg(long, hide = true)]
    pub chaos_seed: Option<u64>,

    /// C++ parity-compatible output: use original 25 columns with masked
    /// attributes (15 baseline bits only). For SHA256 verification against
    /// C++ golden baseline.
    #[arg(long)]
    pub parity_compat: bool,

    /// NTFS reserved cluster bytes to add to root directory's `Size on Disk`.
    ///
    /// C++ adds `(TotalReserved + MftZoneEnd - MftZoneStart) *
    /// BytesPerCluster` to the root. This flag lets parity verification pass
    /// the same value when reading from offline `.iocp` captures that don't
    /// embed volume metadata.
    #[arg(long, hide = true)]
    pub reserved_allocated: Option<u64>,
}

/// Available CLI subcommands.
///
/// Note: Search is NOT a subcommand - it's the default action.
/// This matches ripgrep/fd/Everything patterns where the tool name IS the
/// search.
#[derive(Subcommand)]
pub enum Commands {
    /// Build an index from drive MFT(s)
    ///
    /// By default, indexes ALL available NTFS drives. Use --drive or --drives
    /// to limit to specific drives.
    ///
    /// If no extension is provided, defaults to `.parquet`.
    ///
    /// Examples:
    ///   uffs index index.parquet           # Index ALL drives
    ///   uffs index -d C index.parquet      # Index only C: drive
    ///   uffs index --drives C,D,E out.parquet  # Index C:, D:, E:
    ///   uffs index myindex                 # Creates myindex.parquet
    Index {
        /// Output file path (extension defaults to .parquet)
        output: PathBuf,

        /// Drive letter to index (limits to single drive)
        #[arg(short, long, conflicts_with = "drives", value_parser = parse_drive_letter)]
        drive: Option<char>,

        /// Multiple drive letters to index (e.g., C,D,E)
        #[arg(long, value_delimiter = ',', conflicts_with = "drive", value_parser = parse_drive_letter)]
        drives: Option<Vec<char>>,
    },

    /// Show information about an index file
    Info {
        /// Index file path
        path: PathBuf,
    },

    /// Show statistics about files in an index
    ///
    /// Without a path, connects to the daemon and runs the `overview`
    /// aggregate preset. With a path, loads a parquet index file.
    Stats {
        /// Index file path (optional; omit to use daemon)
        path: Option<PathBuf>,

        /// Show top N largest files (parquet mode only)
        #[arg(long, default_value = "10")]
        top: u32,
    },

    /// Run aggregate analytics on the filesystem index
    ///
    /// Runs one of the built-in presets or a custom aggregation spec
    /// against all loaded drives in the daemon. No rows are returned —
    /// only aggregate results.
    ///
    /// Examples:
    ///   `uffs aggregate overview`          # Full filesystem overview
    ///   `uffs aggregate by_extension`      # Top 50 extensions
    ///   `uffs aggregate by_type`           # Breakdown by file type
    ///   `uffs aggregate by_drive`          # Per-drive totals
    ///   `uffs aggregate by_size`           # Size distribution
    ///   `uffs aggregate by_age`            # Age distribution
    ///   `uffs aggregate count`             # Simple total count
    #[command(alias = "agg")]
    Aggregate {
        /// Preset name or aggregation kind.
        ///
        /// Available presets: `overview`, `by_type`, `by_extension`,
        /// `by_drive`, `by_size`, `by_age`.
        ///
        /// Special kind: count (returns total matching count only).
        preset: String,

        /// Output format: table (default), json, csv, tsv.
        #[arg(long, default_value = "table")]
        format: String,
    },

    /// Manage the UFFS background daemon
    ///
    /// The daemon runs automatically when you search. Use this subcommand
    /// to explicitly start it, check its status, stop it, or force a restart.
    ///
    /// Examples:
    ///   uffs daemon start --data-dir ~/`uffs_data`
    ///   uffs daemon status
    ///   uffs daemon stats
    ///   uffs daemon stop
    ///   uffs daemon restart
    Daemon {
        /// Daemon management action.
        #[command(subcommand)]
        action: DaemonAction,
    },
}

/// Actions for `uffs daemon` subcommand.
#[derive(Subcommand)]
pub enum DaemonAction {
    /// Start the daemon with specified data sources.
    ///
    /// On Windows, live NTFS drives are auto-discovered if no MFT files
    /// are provided.  On macOS/Linux, provide --mft-file or --data-dir.
    Start {
        /// Raw MFT file(s) to load (comma-separated).
        #[arg(long, value_delimiter = ',')]
        mft_file: Vec<PathBuf>,

        /// Data directory containing `drive_*` subdirectories with MFT files.
        #[arg(long)]
        data_dir: Option<PathBuf>,

        /// Skip the file cache and always re-parse MFT files.
        #[arg(long)]
        no_cache: bool,
    },
    /// Show daemon status (running, loading, drives loaded, PID).
    Status,
    /// Show performance statistics (queries, timing, startup duration).
    Stats,
    /// Gracefully stop the running daemon.
    Stop,
    /// Stop the daemon and remove its PID/socket files (hard kill).
    Kill,
    /// Stop then restart the daemon (re-loads all indices).
    Restart,

    /// Run the daemon in-process (used internally by auto-start).
    ///
    /// This is the embedded daemon entry point — same functionality as the
    /// standalone `uffs-daemon` binary.  Normally invoked by the client
    /// library's auto-start logic; not intended for direct user use.
    #[command(hide = true)]
    Run {
        /// Raw MFT file(s) to load.
        #[arg(long = "mft-file", value_delimiter = ',')]
        mft_files: Vec<PathBuf>,

        /// Data directory containing `drive_*` subdirectories.
        #[arg(long = "data-dir")]
        data_dir: Option<PathBuf>,

        /// Live drive letters (Windows only).
        #[arg(long = "drive")]
        drives: Vec<char>,

        /// Idle timeout in seconds (default 600).
        #[arg(long, default_value = "600")]
        idle_timeout: u64,

        /// Disable auto-retire.
        #[arg(long)]
        no_retire: bool,

        /// Skip cache.
        #[arg(long)]
        no_cache: bool,

        /// Log level.
        #[arg(long, default_value = "info")]
        log_level: String,
    },
}
