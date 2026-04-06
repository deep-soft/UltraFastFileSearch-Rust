//! Tests for canonical field identifiers.

#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::print_stdout,
    clippy::use_debug,
    clippy::min_ident_chars
)]

use super::*;

#[test]
fn field_id_parse_accepts_common_aliases() {
    let cases = [
        ("drv", FieldId::Drive),
        ("path only", FieldId::PathOnly),
        ("allocated", FieldId::SizeOnDisk),
        ("written", FieldId::Modified),
        ("ext", FieldId::Extension),
        ("directory", FieldId::Type),
        ("r", FieldId::ReadOnly),
        ("notcontent", FieldId::NotIndexed),
        ("decendents", FieldId::Descendants),
        ("parityattributes", FieldId::ParityAttributes),
    ];

    for (input, expected) in cases {
        assert_eq!(FieldId::parse(input), Some(expected), "alias '{input}'");
    }
}

#[test]
fn field_id_metadata_round_trips_canonical_names() {
    for &field in FieldId::ALL {
        let meta = field.metadata();
        assert_eq!(FieldId::parse(meta.canonical_name), Some(field));
        assert!(meta.projectable || meta.filterable || meta.sortable);
    }
}

#[test]
fn field_id_metadata_captures_default_sort_direction() {
    assert_eq!(
        FieldId::Name.metadata().default_sort_direction,
        Some(SortDirection::Ascending)
    );
    assert_eq!(
        FieldId::Size.metadata().default_sort_direction,
        Some(SortDirection::Descending)
    );
    // Boolean attribute fields default to descending (flagged first).
    assert_eq!(
        FieldId::ReadOnly.metadata().default_sort_direction,
        Some(SortDirection::Descending)
    );
    // Non-sortable fields have no default direction.
    assert_eq!(
        FieldId::ParityAttributes.metadata().default_sort_direction,
        None
    );
}

#[test]
fn field_id_sortable_matches_metadata() {
    assert!(FieldId::Size.metadata().sortable);
    assert!(FieldId::Descendants.metadata().sortable);
    // Boolean attribute fields are sortable (groups true/false via
    // field_to_attr_bit).
    assert!(FieldId::ReadOnly.metadata().sortable);
    assert!(FieldId::Hidden.metadata().sortable);
    assert!(FieldId::System.metadata().sortable);
    assert!(FieldId::Compressed.metadata().sortable);
    assert!(FieldId::DirectoryFlag.metadata().sortable);
    // Non-sortable fields.
    assert!(!FieldId::ParityAttributes.metadata().sortable);
}

#[test]
fn field_id_presentation_fields_non_empty_for_projectable() {
    for &field in FieldId::ALL {
        let meta = field.metadata();
        if meta.projectable {
            assert!(
                !meta.display_name.is_empty(),
                "projectable field {field:?} has empty display_name",
            );
            assert!(
                !meta.tui_label.is_empty(),
                "projectable field {field:?} has empty tui_label",
            );
        }
    }
}

#[test]
fn field_id_cycle_next_wraps_around() {
    let first = FieldId::SORT_CYCLE[0];
    let last = *FieldId::SORT_CYCLE.last().unwrap();
    assert_eq!(last.cycle_next(), first);
}

#[test]
fn field_id_nearest_sort_maps_non_sortable_to_name() {
    assert_eq!(FieldId::ReadOnly.nearest_sort_field(), FieldId::Name);
    assert_eq!(FieldId::Hidden.nearest_sort_field(), FieldId::Name);
}

#[test]
fn field_id_tree_field_detection() {
    assert!(FieldId::Descendants.is_tree_field());
    assert!(FieldId::TreeSize.is_tree_field());
    assert!(FieldId::TreeAllocated.is_tree_field());
    assert!(FieldId::Bulkiness.is_tree_field());
    assert!(!FieldId::Size.is_tree_field());
}

// ── Aggregate capability tests ─────────────────────────────────

#[test]
fn aggregate_capability_table() {
    // This test IS the generated capability table. Run with --nocapture
    // to see it. The printed output is the authoritative reference for
    // which fields support which aggregation operations.
    println!();
    println!(
        "{:<22} {:>6} {:>5} {:>6} {:>7} {:>10} {:>3}",
        "Field", "Type", "Agg", "Group", "Bucket", "Cardinality", "Top"
    );
    println!("{}", "-".repeat(65));
    for &field in FieldId::ALL {
        let m = field.metadata();
        let a = &m.aggregate;
        println!(
            "{:<22} {:>6} {:>5} {:>6} {:>7} {:>10} {:>3}",
            m.canonical_name,
            format!("{:?}", m.field_type),
            if a.aggregatable { "yes" } else { "-" },
            if a.groupable { "yes" } else { "-" },
            if a.bucket_support { "yes" } else { "-" },
            format!("{:?}", a.cardinality),
            if a.default_top > 0 {
                format!("{}", a.default_top)
            } else {
                "-".to_owned()
            },
        );
    }
    println!("{}", "-".repeat(65));

    // Summary counts
    let total = FieldId::ALL.len();
    let agg = FieldId::ALL
        .iter()
        .filter(|f| f.metadata().aggregate.aggregatable)
        .count();
    let grp = FieldId::ALL
        .iter()
        .filter(|f| f.metadata().aggregate.groupable)
        .count();
    let bkt = FieldId::ALL
        .iter()
        .filter(|f| f.metadata().aggregate.bucket_support)
        .count();
    println!("Total: {total}  Aggregatable: {agg}  Groupable: {grp}  Bucketable: {bkt}");
}

#[test]
fn every_field_has_valid_aggregate_meta() {
    for &field in FieldId::ALL {
        let meta = field.metadata();
        let agg = &meta.aggregate;

        // Numeric and Timestamp fields should be aggregatable.
        if matches!(meta.field_type, FieldType::Numeric | FieldType::Timestamp) {
            assert!(
                agg.aggregatable,
                "{field:?} is {:?} but not aggregatable",
                meta.field_type
            );
        }

        // Bool fields should be groupable (they make ideal facets).
        if meta.field_type == FieldType::Bool {
            assert!(agg.groupable, "{field:?} is Bool but not groupable");
            assert_eq!(
                agg.cardinality,
                Cardinality::Fixed,
                "{field:?} is Bool but cardinality is not Fixed",
            );
        }

        // Groupable fields must have default_top > 0.
        if agg.groupable {
            assert!(
                agg.default_top > 0,
                "{field:?} is groupable but default_top is 0",
            );
        }

        // Non-groupable fields should have default_top == 0.
        if !agg.groupable {
            assert_eq!(
                agg.default_top, 0,
                "{field:?} is not groupable but has default_top={}",
                agg.default_top
            );
        }

        // Bucket support should only be on numeric/timestamp fields.
        if agg.bucket_support {
            assert!(
                matches!(meta.field_type, FieldType::Numeric | FieldType::Timestamp),
                "{field:?} has bucket_support but is {:?}",
                meta.field_type
            );
        }

        // Fixed cardinality should only be on Bool/Enum fields.
        if agg.cardinality == Cardinality::Fixed {
            assert!(
                matches!(meta.field_type, FieldType::Bool | FieldType::Enum),
                "{field:?} has Fixed cardinality but is {:?}",
                meta.field_type
            );
        }
    }
}

#[test]
fn aggregate_bool_fields_are_facets() {
    // All 19 bool attribute fields + DirectoryFlag should be groupable
    // with Fixed cardinality and default_top=2.
    let bool_fields: Vec<_> = FieldId::ALL
        .iter()
        .filter(|f| f.metadata().field_type == FieldType::Bool)
        .collect();

    assert!(
        bool_fields.len() >= 19,
        "Expected at least 19 bool fields, got {}",
        bool_fields.len()
    );

    for &&field in &bool_fields {
        let a = field.metadata().aggregate;
        assert!(a.groupable, "{field:?} is Bool but not groupable");
        assert_eq!(a.cardinality, Cardinality::Fixed, "{field:?}");
        assert_eq!(a.default_top, 2, "{field:?}");
        assert!(!a.aggregatable, "{field:?} Bool should not be aggregatable");
        assert!(
            !a.bucket_support,
            "{field:?} Bool should not have bucket support"
        );
    }
}

#[test]
fn aggregate_numeric_fields_are_aggregatable_and_bucketable() {
    let numeric_fields: Vec<_> = FieldId::ALL
        .iter()
        .filter(|f| f.metadata().field_type == FieldType::Numeric)
        .collect();

    assert!(
        numeric_fields.len() >= 8,
        "Expected at least 8 numeric fields, got {}",
        numeric_fields.len()
    );

    for &&field in &numeric_fields {
        let a = field.metadata().aggregate;
        assert!(a.aggregatable, "{field:?} is Numeric but not aggregatable");
        assert!(a.bucket_support, "{field:?} is Numeric but not bucketable");
        assert!(!a.groupable, "{field:?} Numeric should not be groupable");
    }
}

#[test]
fn aggregate_timestamp_fields_are_aggregatable_and_bucketable() {
    let ts_fields = [FieldId::Created, FieldId::Modified, FieldId::Accessed];
    for field in ts_fields {
        let a = field.metadata().aggregate;
        assert!(a.aggregatable, "{field:?}");
        assert!(a.bucket_support, "{field:?}");
        assert!(!a.groupable, "{field:?} Timestamp should not be groupable");
    }
}

#[test]
fn aggregate_key_fields_have_correct_cardinality() {
    assert_eq!(
        FieldId::Drive.metadata().aggregate.cardinality,
        Cardinality::Fixed
    );
    assert_eq!(
        FieldId::Type.metadata().aggregate.cardinality,
        Cardinality::Low
    );
    assert_eq!(
        FieldId::Extension.metadata().aggregate.cardinality,
        Cardinality::Medium
    );
    assert_eq!(
        FieldId::Name.metadata().aggregate.cardinality,
        Cardinality::Unbounded
    );
    assert_eq!(
        FieldId::PathOnly.metadata().aggregate.cardinality,
        Cardinality::Unbounded
    );
}

#[test]
fn aggregate_non_aggregatable_fields() {
    // Path, Attributes, AttributeValue, ParityAttributes should not be
    // aggregatable, groupable, or bucketable.
    let inert = [
        FieldId::Path,
        FieldId::Attributes,
        FieldId::AttributeValue,
        FieldId::ParityAttributes,
    ];
    for field in inert {
        let a = field.metadata().aggregate;
        assert!(!a.aggregatable, "{field:?}");
        assert!(!a.groupable, "{field:?}");
        assert!(!a.bucket_support, "{field:?}");
    }
}
