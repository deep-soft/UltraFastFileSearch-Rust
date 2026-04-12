// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! Search engine: backend, sort, filters, query routing, tree walk.
//!
//! This module contains the compact-index search infrastructure extracted
//! from `uffs-tui` so it can be shared between the TUI, daemon, and any
//! future surface.

pub mod backend;
pub mod columns;
pub mod derived;
pub mod field;
pub mod filters;
pub mod query;
mod sorting;
pub mod tree;
