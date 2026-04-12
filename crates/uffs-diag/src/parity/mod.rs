// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! Parity comparison helpers for UFFS scan output validation.
//!
//! This module provides data structures and functions for comparing
//! reference and Rust UFFS scan outputs.

mod stats;

pub use stats::{ComparisonResults, FieldStats};
