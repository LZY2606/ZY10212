use ultrasonic_adjudication::fixture;
use ultrasonic_adjudication::model::boot;

#[test]
fn detector_fixture_classes() {
    let w = boot(
        1,
        "amp",
        fixture::DEFAULT_THRESHOLD_AMP,
        fixture::DEFAULT_THRESHOLD_SNR,
    )
    .unwrap();
    for sv in &w.scans {
        eprintln!("scan {} surface={}", sv.id, sv.surface_sample);
        for cid in &sv.candidates {
            let c = w.candidates.iter().find(|c| &c.id == cid).unwrap();
            eprintln!(
                "  {} gate={:8} kind={:10?} peak={} [{:4},{:4}) amp={:7.0}{} subs={} depth={:.2}",
                c.id,
                c.gate_kind,
                c.kind,
                c.peak,
                c.start,
                c.end,
                c.amplitude,
                if c.lower_bound { ">=" } else { "   " },
                c.sub_peaks.len(),
                w.live_depth_mm(c.scan_id, c.peak).unwrap_or(f64::NAN)
            );
        }
    }
    assert!(!w.inverted);
}
