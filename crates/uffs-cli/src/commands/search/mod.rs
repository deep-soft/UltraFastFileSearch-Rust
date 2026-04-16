// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! Search command — thin-client output helpers.
//!
//! All searches route through the UFFS daemon via `search_cli` RPC.
//! This module provides output formatting for the responses.

/// Output dispatch and formatting.
pub mod dispatch;
