// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! Protocol round-trip and serde tests.

#![expect(
    clippy::indexing_slicing,
    reason = "test code — indices are verified by test assertions"
)]

use super::*;

/// D2.2.5: serialize/deserialize round-trip for request.
#[test]
fn request_round_trip() {
    let req = RpcRequest::new(1, "search", Some(serde_json::json!({"pattern": "*.rs"})));
    let json = serde_json::to_string(&req).expect("serialize");
    let parsed: RpcRequest = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(parsed.method, "search");
    assert_eq!(parsed.id, Some(1));
}

/// D2.2.5: serialize/deserialize round-trip for response.
#[test]
fn response_round_trip() {
    let resp = RpcResponse::success(
        42,
        serde_json::json!({"rows": [], "records_scanned": 0_u64}),
    );
    let json = serde_json::to_string(&resp).expect("serialize");
    let parsed: RpcResponse = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(parsed.id, 42);
}

/// D2.2.5: serialize/deserialize round-trip for error.
#[test]
fn error_round_trip() {
    let err = RpcErrorResponse::error(Some(1), ERR_METHOD_NOT_FOUND, "Method not found");
    let json = serde_json::to_string(&err).expect("serialize");
    let parsed: RpcErrorResponse = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(parsed.error.code, ERR_METHOD_NOT_FOUND);
}

/// D2.2.5: `SearchParams` serialize/deserialize.
#[test]
fn search_params_round_trip() {
    let params = SearchParams {
        pattern: "*.rs".to_owned(),
        case_sensitive: true,
        sorts: vec![SearchSortSpec {
            field: "size".to_owned(),
            direction: Some(SearchSortDirection::Desc),
        }],
        limit: Some(100),
        filter_mode: Some(SearchFilterMode::Files),
        projection: vec!["path".to_owned(), "size".to_owned()],
        response_mode: Some(SearchResponseMode::Json),
        ..Default::default()
    };
    let json = serde_json::to_value(&params).expect("serialize");
    let parsed: SearchParams = serde_json::from_value(json).expect("deserialize");
    assert_eq!(parsed.pattern, "*.rs");
    assert!(parsed.case_sensitive);
    assert_eq!(parsed.limit, Some(100));
    assert_eq!(parsed.sorts.len(), 1);
    assert_eq!(parsed.filter_mode, Some(SearchFilterMode::Files));
    assert_eq!(parsed.response_mode, Some(SearchResponseMode::Json));
}

/// Canonical helpers preserve legacy single-flag sort semantics.
///
/// First field: ascending by default (no `--sort-desc`).
/// Secondary fields: field-type defaults (numeric → desc, string → asc).
/// `--sort-desc` flag flips the first field to descending.
/// `-` prefix forces descending on any individual field.
#[test]
fn canonicalize_legacy_sort_preserves_primary_sort_desc_override() {
    // --sort size,name (no --sort-desc) → first=asc, second=field default
    let specs = SearchParams::canonicalize_legacy_sort("size,name", false);
    assert_eq!(specs.len(), 2);
    assert_eq!(specs[0].field, "size");
    assert_eq!(
        specs[0].direction,
        Some(SearchSortDirection::Asc),
        "first field defaults to asc without --sort-desc"
    );
    assert_eq!(specs[1].field, "name");
    assert_eq!(
        specs[1].direction,
        Some(SearchSortDirection::Asc),
        "name (string) defaults to asc"
    );

    // --sort name --sort-desc → first field flipped to desc
    let desc_specs = SearchParams::canonicalize_legacy_sort("name", true);
    assert_eq!(desc_specs[0].direction, Some(SearchSortDirection::Desc));
}

/// `-` prefix forces descending on individual sort fields.
#[test]
fn canonicalize_legacy_sort_dash_prefix_descending() {
    // -modified,name → modified=desc, name=asc(default)
    let specs = SearchParams::canonicalize_legacy_sort("-modified,name", false);
    assert_eq!(specs.len(), 2);
    assert_eq!(specs[0].field, "modified");
    assert_eq!(
        specs[0].direction,
        Some(SearchSortDirection::Desc),
        "dash prefix forces descending"
    );
    assert_eq!(specs[1].field, "name");
    assert_eq!(
        specs[1].direction,
        Some(SearchSortDirection::Asc),
        "name defaults to asc"
    );

    // -size alone
    let single = SearchParams::canonicalize_legacy_sort("-size", false);
    assert_eq!(single.len(), 1);
    assert_eq!(single[0].field, "size");
    assert_eq!(single[0].direction, Some(SearchSortDirection::Desc));
}

/// Secondary numeric fields use field-type defaults (desc for
/// size/time/descendants).
#[test]
fn canonicalize_legacy_sort_secondary_field_defaults() {
    let specs = SearchParams::canonicalize_legacy_sort("name,size,modified", false);
    assert_eq!(specs.len(), 3);
    assert_eq!(
        specs[0].direction,
        Some(SearchSortDirection::Asc),
        "first field = asc"
    );
    assert_eq!(
        specs[1].direction,
        Some(SearchSortDirection::Desc),
        "secondary size defaults to desc"
    );
    assert_eq!(
        specs[2].direction,
        Some(SearchSortDirection::Desc),
        "secondary modified defaults to desc"
    );
}

/// Canonical helpers prefer the new filter field over the legacy one.
#[test]
fn resolved_filter_mode_prefers_canonical_field() {
    let params = SearchParams {
        filter: Some("dirs".to_owned()),
        filter_mode: Some(SearchFilterMode::Files),
        ..Default::default()
    };

    assert_eq!(params.resolved_filter_mode(), SearchFilterMode::Files);
}

/// D2.2.5: `DaemonStatus` serialize/deserialize.
#[test]
fn daemon_status_round_trip() {
    let loading = DaemonStatus::Loading {
        drives_loaded: 3,
        drives_total: 7,
    };
    let json = serde_json::to_string(&loading).expect("serialize");
    let parsed: DaemonStatus = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(parsed, loading);

    let ready = DaemonStatus::Ready;
    let ready_json = serde_json::to_string(&ready).expect("serialize");
    let ready_parsed: DaemonStatus = serde_json::from_str(&ready_json).expect("deserialize");
    assert_eq!(ready_parsed, ready);
}

/// D2.2.5: `SearchResponse` with rows.
#[test]
fn search_response_round_trip() {
    let resp = SearchResponse {
        rows: vec![SearchRow {
            drive: 'C',
            path: "C:\\test.rs".to_owned(),
            name: "test.rs".to_owned(),
            size: 1024,
            is_directory: false,
            modified: 1_700_000_000_000_000,
            created: 1_700_000_000_000_000,
            accessed: 1_700_000_000_000_000,
            flags: 0x20,
            allocated: 4096,
            descendants: 0,
            treesize: 0,
            tree_allocated: 0,
        }],
        total_count: 1,
        records_scanned: 1_000_000,
        duration_ms: 8,
        truncated: false,
        shmem_path: None,
        shmem_count: None,
        profile: None,
        applied_sorts: vec![SearchSortSpec {
            field: "modified".to_owned(),
            direction: Some(SearchSortDirection::Desc),
        }],
        applied_projection: vec!["path".to_owned(), "size".to_owned()],
        response_mode: Some(SearchResponseMode::Rows),
        projected_rows: Some(vec![serde_json::Map::from_iter([
            (
                "path".to_owned(),
                serde_json::Value::String("C:\\test.rs".to_owned()),
            ),
            ("size".to_owned(), serde_json::Value::from(1024_u64)),
        ])]),
        aggregations: vec![],
    };
    let json = serde_json::to_string(&resp).expect("serialize");
    let parsed: SearchResponse = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(parsed.rows.len(), 1);
    let first_row = parsed.rows.first().expect("at least one row");
    assert_eq!(first_row.name, "test.rs");
    assert_eq!(parsed.duration_ms, 8);
    assert_eq!(parsed.applied_sorts.len(), 1);
    assert_eq!(parsed.applied_projection.len(), 2);
    assert!(parsed.projected_rows.is_some());
}

// ── S1C.4 — Aggregate wire type round-trip tests ──────────────────

/// `AggregateSpecWire` round-trip: preset variant.
#[test]
fn aggregate_spec_wire_preset_round_trip() {
    let spec = AggregateSpecWire {
        kind: "preset".to_owned(),
        label: None,
        field: None,
        top: None,
        interval: None,
        calendar: None,
        boundaries: vec![],
        metrics: vec![],
        preset: Some("overview".to_owned()),
        ..AggregateSpecWire::default()
    };
    let json = serde_json::to_string(&spec).expect("serialize");
    let parsed: AggregateSpecWire = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(parsed.kind, "preset");
    assert_eq!(parsed.preset.as_deref(), Some("overview"));
    // Optional fields should be absent in JSON
    assert!(!json.contains("\"label\""));
    assert!(!json.contains("\"field\""));
}

/// `AggregateSpecWire` round-trip: terms variant with all fields.
#[test]
fn aggregate_spec_wire_terms_round_trip() {
    let spec = AggregateSpecWire {
        kind: "terms".to_owned(),
        label: Some("ext_breakdown".to_owned()),
        field: Some("extension".to_owned()),
        top: Some(50),
        interval: None,
        calendar: None,
        boundaries: vec![],
        metrics: vec!["count".to_owned(), "total_bytes".to_owned()],
        preset: None,
        ..AggregateSpecWire::default()
    };
    let json = serde_json::to_string(&spec).expect("serialize");
    let parsed: AggregateSpecWire = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(parsed.kind, "terms");
    assert_eq!(parsed.label.as_deref(), Some("ext_breakdown"));
    assert_eq!(parsed.field.as_deref(), Some("extension"));
    assert_eq!(parsed.top, Some(50));
    assert_eq!(parsed.metrics.len(), 2);
    assert!(parsed.preset.is_none());
}

/// `AggregateSpecWire` round-trip: date histogram variant.
#[test]
fn aggregate_spec_wire_date_histogram_round_trip() {
    let spec = AggregateSpecWire {
        kind: "date_histogram".to_owned(),
        label: Some("modified_monthly".to_owned()),
        field: Some("modified".to_owned()),
        top: None,
        interval: None,
        calendar: Some("month".to_owned()),
        boundaries: vec![],
        metrics: vec!["count".to_owned()],
        preset: None,
        ..AggregateSpecWire::default()
    };
    let json = serde_json::to_string(&spec).expect("serialize");
    let parsed: AggregateSpecWire = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(parsed.kind, "date_histogram");
    assert_eq!(parsed.calendar.as_deref(), Some("month"));
}

/// `AggregateSpecWire` round-trip: range variant with boundaries.
#[test]
fn aggregate_spec_wire_range_round_trip() {
    let spec = AggregateSpecWire {
        kind: "range".to_owned(),
        label: None,
        field: Some("size".to_owned()),
        top: None,
        interval: None,
        calendar: None,
        boundaries: vec![0, 1024, 1_048_576, 1_073_741_824],
        metrics: vec![],
        preset: None,
        ..AggregateSpecWire::default()
    };
    let json = serde_json::to_string(&spec).expect("serialize");
    let parsed: AggregateSpecWire = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(parsed.boundaries, vec![0, 1024, 1_048_576, 1_073_741_824]);
}

/// `StatsWire` round-trip.
#[test]
fn stats_wire_round_trip() {
    let stats = StatsWire {
        count: 10_000,
        sum: 5_000_000,
        min: 0,
        max: 1_000_000,
        avg: 500.0,
        waste_bytes: 200_000,
        waste_pct: 4.0,
    };
    let json = serde_json::to_string(&stats).expect("serialize");
    let parsed: StatsWire = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(parsed.count, 10_000);
    assert_eq!(parsed.sum, 5_000_000);
    assert_eq!(parsed.min, 0);
    assert_eq!(parsed.max, 1_000_000);
    assert!((parsed.avg - 500.0).abs() < f64::EPSILON);
    assert_eq!(parsed.waste_bytes, 200_000);
    assert!((parsed.waste_pct - 4.0).abs() < f64::EPSILON);
}

/// `BucketWire` round-trip: all optional fields present.
#[test]
fn bucket_wire_full_round_trip() {
    let bucket = BucketWire {
        key: "rs".to_owned(),
        count: 500,
        total_bytes: 2_000_000,
        total_allocated: Some(2_500_000),
        avg_size: Some(4_000.0_f64),
        share_count: Some(5.0_f64),
        share_bytes: Some(3.2_f64),
        sample_rows: Vec::new(),
        drilldown: Vec::new(),
        sub_buckets: Vec::new(),
        verified: false,
    };
    let json = serde_json::to_string(&bucket).expect("serialize");
    let parsed: BucketWire = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(parsed.key, "rs");
    assert_eq!(parsed.count, 500);
    assert_eq!(parsed.total_bytes, 2_000_000);
    assert_eq!(parsed.total_allocated, Some(2_500_000));
    assert!((parsed.avg_size.expect("avg_size") - 4000.0).abs() < f64::EPSILON);
    assert!((parsed.share_count.expect("share_count") - 5.0).abs() < f64::EPSILON);
    assert!((parsed.share_bytes.expect("share_bytes") - 3.2).abs() < f64::EPSILON);
}

/// `BucketWire` round-trip: only required fields (optional fields absent in
/// JSON).
#[test]
fn bucket_wire_minimal_round_trip() {
    let json_str = r#"{"key":"doc","count":10,"total_bytes":1024}"#;
    let parsed: BucketWire = serde_json::from_str(json_str).expect("deserialize");
    assert_eq!(parsed.key, "doc");
    assert_eq!(parsed.count, 10);
    assert_eq!(parsed.total_bytes, 1024);
    assert!(parsed.total_allocated.is_none());
    assert!(parsed.avg_size.is_none());
    assert!(parsed.share_count.is_none());
    assert!(parsed.share_bytes.is_none());
    // Re-serialize and verify optional fields are absent
    let re_json = serde_json::to_string(&parsed).expect("re-serialize");
    assert!(!re_json.contains("total_allocated"));
    assert!(!re_json.contains("avg_size"));
}

/// `AggregateResultWire` round-trip: count kind.
#[test]
fn aggregate_result_wire_count_round_trip() {
    let result = AggregateResultWire {
        label: Some("total_count".to_owned()),
        kind: "count".to_owned(),
        field: None,
        value: Some(1_234_567),
        stats: None,
        buckets: vec![],
        other_count: None,
        total_groups: None,
        next_cursor: None,
        exact: None,
        values_complete: None,
    };
    let json = serde_json::to_string(&result).expect("serialize");
    let parsed: AggregateResultWire = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(parsed.kind, "count");
    assert_eq!(parsed.value, Some(1_234_567));
    assert!(parsed.stats.is_none());
    assert!(parsed.buckets.is_empty());
}

/// `AggregateResultWire` round-trip: stats kind with `StatsWire`.
#[test]
fn aggregate_result_wire_stats_round_trip() {
    let result = AggregateResultWire {
        label: Some("size_stats".to_owned()),
        kind: "stats".to_owned(),
        field: Some("size".to_owned()),
        value: None,
        stats: Some(StatsWire {
            count: 100,
            sum: 50_000,
            min: 10,
            max: 9_000,
            avg: 500.0,
            waste_bytes: 1_000,
            waste_pct: 2.0,
        }),
        buckets: vec![],
        other_count: None,
        total_groups: None,
        next_cursor: None,
        exact: None,
        values_complete: None,
    };
    let json = serde_json::to_string(&result).expect("serialize");
    let parsed: AggregateResultWire = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(parsed.kind, "stats");
    let stats = parsed.stats.expect("stats present");
    assert_eq!(stats.count, 100);
    assert_eq!(stats.sum, 50_000);
}

/// `AggregateResultWire` round-trip: terms with buckets + truncation
/// metadata.
#[test]
fn aggregate_result_wire_terms_round_trip() {
    let result = AggregateResultWire {
        label: Some("ext_terms".to_owned()),
        kind: "terms".to_owned(),
        field: Some("extension".to_owned()),
        value: None,
        stats: None,
        buckets: vec![
            BucketWire {
                key: "rs".to_owned(),
                count: 500,
                total_bytes: 2_000_000,
                avg_size: Some(4_000.0_f64),
                ..BucketWire::default()
            },
            BucketWire {
                key: "toml".to_owned(),
                count: 200,
                total_bytes: 50_000,
                avg_size: Some(250.0_f64),
                ..BucketWire::default()
            },
        ],
        other_count: Some(300),
        total_groups: Some(150),
        next_cursor: None,
        exact: None,
        values_complete: None,
    };
    let json = serde_json::to_string(&result).expect("serialize");
    let parsed: AggregateResultWire = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(parsed.kind, "terms");
    assert_eq!(parsed.buckets.len(), 2);
    assert_eq!(parsed.buckets[0].key, "rs");
    assert_eq!(parsed.buckets[1].key, "toml");
    assert_eq!(parsed.other_count, Some(300));
    assert_eq!(parsed.total_groups, Some(150));
}

/// `SearchParams` round-trip with aggregations + `include_rows`.
#[test]
fn search_params_with_aggregations_round_trip() {
    let params = SearchParams {
        pattern: "*.rs".to_owned(),
        aggregations: vec![
            AggregateSpecWire {
                kind: "preset".to_owned(),
                preset: Some("overview".to_owned()),
                ..AggregateSpecWire::default()
            },
            AggregateSpecWire {
                kind: "count".to_owned(),
                label: Some("total".to_owned()),
                ..AggregateSpecWire::default()
            },
        ],
        include_rows: false,
        ..Default::default()
    };
    let json = serde_json::to_value(&params).expect("serialize");
    let parsed: SearchParams = serde_json::from_value(json).expect("deserialize");
    assert_eq!(parsed.aggregations.len(), 2);
    assert!(!parsed.include_rows);
    assert_eq!(parsed.aggregations[0].kind, "preset");
    assert_eq!(parsed.aggregations[0].preset.as_deref(), Some("overview"));
    assert_eq!(parsed.aggregations[1].kind, "count");
    assert_eq!(parsed.aggregations[1].label.as_deref(), Some("total"));
}

/// `SearchResponse` round-trip with aggregations and no rows.
#[test]
fn search_response_with_aggregations_round_trip() {
    let resp = SearchResponse {
        rows: vec![],
        total_count: 0,
        records_scanned: 500_000,
        duration_ms: 12,
        truncated: false,
        shmem_path: None,
        shmem_count: None,
        profile: None,
        applied_sorts: vec![],
        applied_projection: vec![],
        response_mode: None,
        projected_rows: None,
        aggregations: vec![
            AggregateResultWire {
                label: Some("total_count".to_owned()),
                kind: "count".to_owned(),
                field: None,
                value: Some(500_000),
                stats: None,
                buckets: vec![],
                other_count: None,
                total_groups: None,
                next_cursor: None,
                exact: None,
                values_complete: None,
            },
            AggregateResultWire {
                label: Some("type_breakdown".to_owned()),
                kind: "terms".to_owned(),
                field: Some("type".to_owned()),
                value: None,
                stats: None,
                buckets: vec![BucketWire {
                    key: "Document".to_owned(),
                    count: 10_000,
                    total_bytes: 500_000_000,
                    avg_size: Some(50_000.0_f64),
                    share_count: Some(2.0_f64),
                    share_bytes: Some(10.0_f64),
                    ..BucketWire::default()
                }],
                other_count: Some(490_000),
                total_groups: Some(12),
                next_cursor: None,
                exact: None,
                values_complete: None,
            },
        ],
    };
    let json = serde_json::to_string(&resp).expect("serialize");
    let parsed: SearchResponse = serde_json::from_str(&json).expect("deserialize");
    assert!(parsed.rows.is_empty());
    assert_eq!(parsed.aggregations.len(), 2);
    assert_eq!(parsed.aggregations[0].kind, "count");
    assert_eq!(parsed.aggregations[0].value, Some(500_000));
    assert_eq!(parsed.aggregations[1].kind, "terms");
    assert_eq!(parsed.aggregations[1].buckets.len(), 1);
    assert_eq!(parsed.aggregations[1].buckets[0].key, "Document");
    assert_eq!(parsed.aggregations[1].other_count, Some(490_000));
}

/// Deserialize `AggregateSpecWire` from minimal JSON (only required
/// fields).
#[test]
fn aggregate_spec_wire_minimal_json() {
    let json_str = r#"{"kind":"count"}"#;
    let parsed: AggregateSpecWire = serde_json::from_str(json_str).expect("deserialize");
    assert_eq!(parsed.kind, "count");
    assert!(parsed.label.is_none());
    assert!(parsed.field.is_none());
    assert!(parsed.boundaries.is_empty());
    assert!(parsed.metrics.is_empty());
    assert!(parsed.preset.is_none());
}

/// Deserialize `AggregateResultWire` from minimal JSON.
#[test]
fn aggregate_result_wire_minimal_json() {
    let json_str = r#"{"kind":"count","value":42}"#;
    let parsed: AggregateResultWire = serde_json::from_str(json_str).expect("deserialize");
    assert_eq!(parsed.kind, "count");
    assert_eq!(parsed.value, Some(42));
    assert!(parsed.label.is_none());
    assert!(parsed.stats.is_none());
    assert!(parsed.buckets.is_empty());
}

// ── S2G.12: Serde round-trip tests for wire types ─────────────────

#[test]
fn sample_row_wire_round_trip() {
    let mut fields = std::collections::HashMap::new();
    fields.insert("name".to_owned(), "foo.rs".to_owned());
    fields.insert("size".to_owned(), "4096".to_owned());
    let wire = SampleRowWire {
        fields,
        sort_key: Some(4096),
    };
    let json = serde_json::to_string(&wire).expect("serialize");
    let parsed: SampleRowWire = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(&parsed.fields["name"], "foo.rs");
    assert_eq!(&parsed.fields["size"], "4096");
    assert_eq!(parsed.sort_key, Some(4096));
}

#[test]
fn sample_row_wire_no_sort_key() {
    let wire = SampleRowWire {
        fields: std::collections::HashMap::new(),
        sort_key: None,
    };
    let json = serde_json::to_string(&wire).expect("serialize");
    assert!(!json.contains("sort_key"), "sort_key should be omitted");
    let parsed: SampleRowWire = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(parsed.sort_key, None);
}

#[test]
fn drilldown_wire_round_trip() {
    let wire = DrilldownWire {
        field: "extension".to_owned(),
        op: "eq".to_owned(),
        value: serde_json::Value::String("rs".to_owned()),
    };
    let json = serde_json::to_string(&wire).expect("serialize");
    let parsed: DrilldownWire = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(parsed.field, "extension");
    assert_eq!(parsed.op, "eq");
    assert_eq!(parsed.value, serde_json::Value::String("rs".to_owned()));
}

#[test]
fn drilldown_wire_numeric_value() {
    let wire = DrilldownWire {
        field: "size".to_owned(),
        op: "gte".to_owned(),
        value: serde_json::Value::Number(1_024_i64.into()),
    };
    let json = serde_json::to_string(&wire).expect("serialize");
    let parsed: DrilldownWire = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(parsed.value, serde_json::Value::Number(1_024_i64.into()));
}

#[test]
fn bucket_wire_with_samples_round_trip() {
    let mut fields = std::collections::HashMap::new();
    fields.insert("name".to_owned(), "bar.rs".to_owned());
    let bucket = BucketWire {
        key: "rs".to_owned(),
        count: 100,
        total_bytes: 50_000,
        total_allocated: None,
        avg_size: None,
        share_count: None,
        share_bytes: None,
        sample_rows: vec![SampleRowWire {
            fields,
            sort_key: Some(999),
        }],
        drilldown: vec![DrilldownWire {
            field: "extension".to_owned(),
            op: "eq".to_owned(),
            value: serde_json::Value::String("rs".to_owned()),
        }],
        sub_buckets: Vec::new(),
        verified: false,
    };
    let json = serde_json::to_string(&bucket).expect("serialize");
    assert!(json.contains("sample_rows"));
    assert!(json.contains("drilldown"));
    let parsed: BucketWire = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(parsed.sample_rows.len(), 1);
    assert_eq!(parsed.drilldown.len(), 1);
    assert_eq!(&parsed.sample_rows[0].fields["name"], "bar.rs");
    assert_eq!(parsed.drilldown[0].field, "extension");
}

#[test]
fn bucket_wire_empty_samples_omitted() {
    let bucket = BucketWire {
        key: "txt".to_owned(),
        count: 10,
        total_bytes: 1000,
        ..BucketWire::default()
    };
    let json = serde_json::to_string(&bucket).expect("serialize");
    assert!(
        !json.contains("sample_rows"),
        "empty sample_rows should be omitted"
    );
    assert!(
        !json.contains("drilldown"),
        "empty drilldown should be omitted"
    );
}

#[test]
fn bucket_wire_backward_compat_no_sample_fields() {
    // Old JSON without sample_rows/drilldown should deserialize fine.
    let json_str = r#"{"key":"rs","count":50,"total_bytes":1000}"#;
    let parsed: BucketWire = serde_json::from_str(json_str).expect("deserialize");
    assert_eq!(parsed.key, "rs");
    assert_eq!(parsed.count, 50);
    assert!(parsed.sample_rows.is_empty());
    assert!(parsed.drilldown.is_empty());
}

#[test]
fn aggregate_spec_wire_with_sample() {
    let spec = AggregateSpecWire {
        kind: "terms".to_owned(),
        field: Some("extension".to_owned()),
        top: Some(10),
        sample: Some(3),
        sample_sort: Some("size".to_owned()),
        sample_desc: Some(true),
        ..AggregateSpecWire::default()
    };
    let json = serde_json::to_string(&spec).expect("serialize");
    assert!(json.contains(r#""sample":3"#));
    assert!(json.contains(r#""sample_sort":"size""#));
    let parsed: AggregateSpecWire = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(parsed.sample, Some(3));
    assert_eq!(parsed.sample_sort.as_deref(), Some("size"));
    assert_eq!(parsed.sample_desc, Some(true));
}

#[test]
fn aggregate_spec_wire_no_sample_backward_compat() {
    // Old JSON without sample fields should deserialize fine.
    let json_str = r#"{"kind":"terms","field":"extension","top":10}"#;
    let parsed: AggregateSpecWire = serde_json::from_str(json_str).expect("deserialize");
    assert_eq!(parsed.kind, "terms");
    assert!(parsed.sample.is_none());
    assert!(parsed.sample_sort.is_none());
    assert!(parsed.sample_desc.is_none());
}

// ── Cursor pagination serde ─────────────────────────────────────

/// `SearchParams` with `agg_cursor` and `agg_page_size` round-trips
/// correctly; fields are omitted from JSON when `None`.
#[test]
fn search_params_cursor_pagination_round_trip() {
    let params = SearchParams {
        pattern: "*".to_owned(),
        agg_cursor: Some("0:100:50".to_owned()),
        agg_page_size: Some(50),
        ..Default::default()
    };
    let json = serde_json::to_string(&params).expect("serialize");
    assert!(json.contains(r#""agg_cursor":"0:100:50""#));
    assert!(json.contains(r#""agg_page_size":50"#));

    let parsed: SearchParams = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(parsed.agg_cursor.as_deref(), Some("0:100:50"));
    assert_eq!(parsed.agg_page_size, Some(50));
}

/// `SearchParams` omits cursor fields from JSON when they are `None`.
#[test]
fn search_params_cursor_fields_omitted_when_none() {
    let params = SearchParams {
        pattern: "*.rs".to_owned(),
        ..Default::default()
    };
    let json = serde_json::to_string(&params).expect("serialize");
    assert!(!json.contains("agg_cursor"));
    assert!(!json.contains("agg_page_size"));
}

/// `AggregateResultWire` with a `next_cursor` value round-trips correctly.
#[test]
fn aggregate_result_wire_next_cursor_round_trip() {
    let result = AggregateResultWire {
        label: Some("ext_terms".to_owned()),
        kind: "buckets".to_owned(),
        field: Some("extension".to_owned()),
        value: None,
        stats: None,
        buckets: vec![BucketWire {
            key: "rs".to_owned(),
            count: 500,
            total_bytes: 2_000_000,
            avg_size: Some(4_000.0_f64),
            ..BucketWire::default()
        }],
        other_count: Some(300),
        total_groups: Some(150),
        next_cursor: Some("0:50:50".to_owned()),
        exact: None,
        values_complete: None,
    };
    let json = serde_json::to_string(&result).expect("serialize");
    assert!(json.contains(r#""next_cursor":"0:50:50""#));

    let parsed: AggregateResultWire = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(parsed.next_cursor.as_deref(), Some("0:50:50"));
}

/// `AggregateResultWire` omits `next_cursor` from JSON when `None`.
#[test]
fn aggregate_result_wire_next_cursor_omitted_when_none() {
    let result = AggregateResultWire {
        label: None,
        kind: "count".to_owned(),
        field: None,
        value: Some(42),
        stats: None,
        buckets: vec![],
        other_count: None,
        total_groups: None,
        next_cursor: None,
        exact: None,
        values_complete: None,
    };
    let json = serde_json::to_string(&result).expect("serialize");
    assert!(!json.contains("next_cursor"));
}

/// `exact` and `values_complete` round-trip through JSON.
#[test]
fn aggregate_result_wire_exact_and_values_complete_round_trip() {
    let result = AggregateResultWire {
        label: None,
        kind: "buckets".to_owned(),
        field: Some("extension".to_owned()),
        value: None,
        stats: None,
        buckets: vec![],
        other_count: Some(0),
        total_groups: Some(5),
        next_cursor: None,
        exact: Some(true),
        values_complete: Some(true),
    };
    let json = serde_json::to_string(&result).expect("serialize");
    assert!(json.contains(r#""exact":true"#), "json: {json}");
    assert!(json.contains(r#""values_complete":true"#), "json: {json}");
    let parsed: AggregateResultWire = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(parsed.exact, Some(true));
    assert_eq!(parsed.values_complete, Some(true));
}

/// `exact` and `values_complete` are omitted when `None`.
#[test]
fn aggregate_result_wire_exact_omitted_when_none() {
    let result = AggregateResultWire {
        label: None,
        kind: "count".to_owned(),
        field: None,
        value: Some(42),
        stats: None,
        buckets: vec![],
        other_count: None,
        total_groups: None,
        next_cursor: None,
        exact: None,
        values_complete: None,
    };
    let json = serde_json::to_string(&result).expect("serialize");
    assert!(!json.contains("exact"), "json: {json}");
    assert!(!json.contains("values_complete"), "json: {json}");
}

/// `values_complete` is false when `other_count > 0`.
#[test]
fn aggregate_result_wire_values_complete_false() {
    let result = AggregateResultWire {
        label: None,
        kind: "buckets".to_owned(),
        field: Some("type".to_owned()),
        value: None,
        stats: None,
        buckets: vec![],
        other_count: Some(500),
        total_groups: Some(100),
        next_cursor: None,
        exact: Some(true),
        values_complete: Some(false),
    };
    let json = serde_json::to_string(&result).expect("serialize");
    let parsed: AggregateResultWire = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(parsed.values_complete, Some(false));
    assert_eq!(parsed.exact, Some(true));
}
