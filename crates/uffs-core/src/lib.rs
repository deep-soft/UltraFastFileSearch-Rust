//! # uffs-core: Query Engine for UFFS (Ultra Fast File Search)
//!
//! This crate provides a powerful query engine for searching and filtering
//! MFT data using Polars lazy API.
//!
//! ## Features
//!
//! - **Fluent Query API**: Chain filters naturally
//! - **Pattern Matching**: Glob and regex support with SIMD acceleration
//! - **Path Resolution**: Reconstruct full paths from FRS numbers
//! - **Export Formats**: Table, JSON, CSV output
//!
//! ## Quick Start
//!
//! ```rust,ignore
//! use uffs_mft::MftReader;
//! use uffs_core::MftQuery;
//!
//! #[tokio::main]
//! async fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     // Load MFT data
//!     let df = MftReader::load_parquet("c_drive.parquet")?;
//!
//!     // Query using fluent API
//!     let results = MftQuery::new(df)
//!         .glob("*.rs")
//!         .files_only()
//!         .min_size(1024)
//!         .sort_by_size(true)
//!         .limit(100)
//!         .collect()?;
//!
//!     // Export results
//!     uffs_core::export_table(&results, std::io::stdout())?;
//!
//!     Ok(())
//! }
//! ```

#![forbid(unsafe_code)]
#![warn(clippy::all, clippy::pedantic)]

// Suppress unused crate warnings for deps used by sub-modules or reserved
use devicons as _;
use memchr as _;
use tokio as _;
#[cfg(test)]
use criterion as _;

// ============================================================================
// Module declarations
// ============================================================================

pub mod compact;
pub mod compact_reader;
pub mod compiled_pattern;
mod error;
mod export;
pub mod extensions;
pub mod format;
pub mod glob;
pub mod index_search;
pub mod output;
mod path_resolver;
pub mod pattern;
mod query;
pub mod search;
pub mod tree;
pub mod trigram;

// ============================================================================
// Public API re-exports
// ============================================================================

pub use compiled_pattern::{CompiledPattern, GlobKind, classify_glob, compile_pattern};
pub use error::{CoreError, Result};
pub use export::{export_csv, export_json, export_table};
pub use extensions::{
    ExtensionFilter, ExtensionIndex, ExtensionIndexStats, add_ext_column, ext_expr, has_ext_column,
};
pub use index_search::{
    IndexPattern, IndexQuery, QueryComplexity, QueryFeatures, QueryMode, SearchResult, TypeFilter,
    analyze_pattern_complexity, compile_extensions, compile_index_pattern, compile_parsed_pattern,
};
#[expect(
    deprecated,
    reason = "re-exporting deprecated function for backward compatibility"
)]
pub use path_resolver::add_path_column_multi_drive;
pub use path_resolver::{
    FastPathResolver, FastPathResolverMultiDrive, FastPathResolverStats, NameArena, PathResolver,
    add_path_only_column, add_paths_from_full_data,
};
pub use query::MftQuery;
pub use tree::{TreeColumn, TreeIndex, add_tree_columns, apply_directory_treesize};
// Re-export commonly used types
pub use uffs_mft::FileFlags;
pub use uffs_polars::{DataFrame, LazyFrame, columns};
