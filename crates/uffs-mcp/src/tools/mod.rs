//! MCP tool implementations.
//!
//! Each tool module contains a handler function that takes parsed arguments
//! and an [`UffsClient`](uffs_client::connect::UffsClient), returning an
//! [`rmcp::model::CallToolResult`].

/// `uffs_aggregate` — server-side aggregation summaries.
pub mod aggregate;
/// `uffs_drives` — list indexed NTFS drives.
pub mod drives;
/// `uffs_facet_values` — search within facet values for a field.
pub mod facet_values;
/// `uffs_info` — file/directory detail lookup by path.
pub mod info;
/// `uffs_search` — file search across all indexed drives.
pub mod search;
/// `uffs_status` — daemon health and loading progress.
pub mod status;
