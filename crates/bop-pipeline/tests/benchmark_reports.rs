//! Stable backend benchmark report calculations.

use std::{collections::BTreeMap, time::Duration};

use bop_core::{BackendInfo, BackendKind};
use bop_pipeline::RenderBenchmarkSummary;

#[test]
fn report_sorts_samples_and_uses_nearest_rank_percentiles() {
    let backend =
        BackendInfo::try_new(BackendKind::Wgpu, "test", None, BTreeMap::default()).unwrap();
    let report = RenderBenchmarkSummary::try_new(
        backend,
        "triangle-32x32",
        vec![
            Duration::from_micros(40),
            Duration::from_micros(10),
            Duration::from_micros(30),
            Duration::from_micros(20),
        ],
    )
    .unwrap();
    assert_eq!(report.sample_count(), 4);
    assert_eq!(report.minimum_seconds().to_bits(), 0.000_01_f32.to_bits());
    assert_eq!(report.median_seconds().to_bits(), 0.000_03_f32.to_bits());
    assert_eq!(report.p95_seconds().to_bits(), 0.000_04_f32.to_bits());
    assert!(
        serde_json::to_string(&report)
            .unwrap()
            .contains("triangle-32x32")
    );
}
