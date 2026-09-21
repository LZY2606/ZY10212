//! Domain model: event-sourced adjudication state, echo detection and depth rules.

use crate::dsp::{hilbert_envelope, local_maxima};
use crate::fixture::{
    scans, Calib, Scan, ScanDef, ADC_MAX, CALIBS, DEFAULT_GATES, FS_MHZ, NOISE_WINDOW,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Gate membership uses left-closed / right-open sample intervals [lo, hi).
#[derive(Clone, Serialize)]
pub struct Gate {
    pub kind: String,
    pub lo: i64,
    pub hi: i64,
}

impl Gate {
    pub fn contains_sample(&self, s: i64) -> bool {
        s >= self.lo && s < self.hi
    }
}

#[derive(Clone, Copy, Serialize, serde::Deserialize, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum CandidateKind {
    /// A single resolvable lobe.
    Single,
    /// Several local maxima inside one lobe that cannot be resolved.
    Composite,
    /// A lobe clipped by the ADC; amplitude is only a lower bound.
    Saturated,
    /// A lobe created by an operator split.
    Split,
}

#[derive(Clone, Copy, Serialize, serde::Deserialize, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    Scatter,
    Surface,
    Bottom,
    Overlap,
}

#[derive(Clone, Serialize)]
pub struct SubPeak {
    pub sample: i64,
    pub amp: f64,
}

#[derive(Clone, Serialize)]
pub struct Candidate {
    pub id: String,
    pub scan_id: i64,
    pub gate_kind: String,
    pub start: i64,
    pub end: i64,
    pub peak: i64,
    pub amplitude: f64,
    pub lower_bound: bool,
    pub kind: CandidateKind,
    pub sub_peaks: Vec<SubPeak>,
    pub verdict: Option<Verdict>,
    pub track_id: Option<i64>,
    pub origin: String, // "auto" | "split"
    /// Depth computed at creation time with the then-current calibration.
    pub depth_snapshot_mm: Option<f64>,
    pub calib_id_snapshot: i64,
    pub locked: bool,
}

#[derive(Clone, Serialize)]
pub struct Track {
    pub id: i64,
    pub candidate_ids: Vec<String>,
}

#[derive(Clone, Serialize)]
pub struct EventRecord {
    pub seq: i64,
    pub at_rfc3339: String,
    pub note: Option<String>,
    #[serde(flatten)]
    pub event: Event,
}

#[derive(Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    Bootstrap {
        calib_id: i64,
        mode: String,
        threshold_amp: f64,
        threshold_snr: f64,
    },
    SurfaceCorrected {
        scan_id: i64,
        sample: i64,
    },
    ThresholdChanged {
        mode: String,
        value: f64,
    },
    GateMoved {
        kind: String,
        lo: i64,
        hi: i64,
    },
    CandidateSplit {
        parent_id: String,
        cut: i64,
    },
    CandidateVerdict {
        candidate_id: String,
        verdict: Verdict,
    },
    TrackGrouped {
        track_id: i64,
        candidate_ids: Vec<String>,
    },
    CalibrationSwitched {
        calib_id: i64,
    },
}

#[derive(Clone, Serialize)]
pub struct ScanView {
    pub id: i64,
    pub x_mm: f64,
    pub y_mm: f64,
    pub recorded_surface_sample: i64,
    pub surface_sample: i64,
    pub raw: Vec<i16>,
    pub envelope: Vec<f64>,
    pub noise_floor: f64,
    pub candidates: Vec<String>,
}

#[derive(Clone, Serialize)]
pub struct World {
    pub calibs: Vec<Calib>,
    pub current_calib_id: i64,
    pub threshold_mode: String, // "amp" | "snr"
    pub threshold_amp: f64,
    pub threshold_snr: f64,
    pub gates: Vec<Gate>,
    pub surfaces: BTreeMap<i64, i64>,
    pub candidates: Vec<Candidate>,
    pub tracks: Vec<Track>,
    pub counters: BTreeMap<String, i64>,
    pub inverted: bool,
    pub inverted_reason: Option<String>,
    pub scans: Vec<ScanView>,
}

#[derive(Clone)]
struct Region {
    start: usize,
    end: usize,
    peak: usize,
    peak_amp: f64,
    saturated: bool,
    sub: Vec<SubPeak>,
    kind: CandidateKind,
}

fn scan_defs() -> Vec<ScanDef> {
    crate::fixture::scan_defs()
}

impl World {
    fn scan(&self, id: i64) -> Option<&ScanView> {
        self.scans.iter().find(|s| s.id == id)
    }

    fn gate_of(&self, sample: i64) -> Option<&Gate> {
        self.gates.iter().find(|g| g.contains_sample(sample))
    }

    pub fn calib(&self) -> Calib {
        *CALIBS
            .iter()
            .find(|c| c.id == self.current_calib_id)
            .unwrap_or(CALIBS.first().unwrap())
    }

    fn next_counter(&mut self, key: &str) -> i64 {
        let v = self.counters.entry(key.to_string()).or_insert(0);
        *v += 1;
        *v
    }

    /// Live depth relative to the corrected surface: d = c/2 * (t_peak - t_surface).
    ///
    /// Probe delay is an instrument constant applied when mapping the absolute
    /// time base; here the corrected surface arrival is the zero-depth
    /// reference, so only the difference of arrival times matters. Every
    /// candidate also stores the calibration used at creation time.
    pub fn live_depth_mm(&self, scan_id: i64, sample: i64) -> Option<f64> {
        let calib = self.calib();
        let surface = *self.surfaces.get(&scan_id)?;
        let dt_s = (sample - surface) as f64 / (FS_MHZ * 1.0e6);
        Some(0.5 * calib.velocity_ms * dt_s * 1000.0)
    }

    fn check_inversion(&mut self) {
        // Depth axis is inverted whenever a bottom-labelled candidate arrives
        // at or before the corrected surface. No positive depth result is then valid.
        let mut reason: Option<String> = None;
        for cand in &self.candidates {
            if cand.verdict == Some(Verdict::Bottom) || cand.gate_kind == "bottom" {
                if let Some(surface) = self.surfaces.get(&cand.scan_id) {
                    if cand.peak <= *surface {
                        reason = Some(format!(
                            "scan {} bottom candidate at sample {} is no later than surface {}",
                            cand.scan_id, cand.peak, surface
                        ));
                        break;
                    }
                }
            }
        }
        self.inverted = reason.is_some();
        self.inverted_reason = reason;
    }
}

fn threshold_value(world: &World, noise: f64) -> f64 {
    if world.threshold_mode == "snr" {
        world.threshold_snr * noise
    } else {
        world.threshold_amp / crate::fixture::ADC_SCALE
    }
}

fn detect_regions(env: &[f64], raw: &[i16], thr: f64) -> Vec<Region> {
    let n = env.len();
    let mut regions = Vec::new();
    let mut i = 0usize;
    while i < n {
        if env[i] < thr {
            i += 1;
            continue;
        }
        let start = i;
        while i < n && env[i] >= thr {
            i += 1;
        }
        let end = i; // half-open
        let mut peak = start;
        for k in start..end {
            if env[k] > env[peak] {
                peak = k;
            }
        }
        let saturated = raw[start..end]
            .iter()
            .any(|v| (*v == ADC_MAX) || (*v == -ADC_MAX));
        let mut sub: Vec<SubPeak> = local_maxima(&env[start..end])
            .into_iter()
            .map(|k| SubPeak {
                sample: (start + k) as i64,
                amp: env[start + k],
            })
            .collect();
        if sub.is_empty() {
            sub.push(SubPeak {
                sample: peak as i64,
                amp: env[peak],
            });
        }
        let kind = if saturated {
            CandidateKind::Saturated
        } else {
            // Ringing side lobes are clearly weaker than the carrier lobe.
            // Unresolved near peaks are two maxima of comparable height
            // (within 10%), closely spaced, with no deep valley between them.
            let mut unresolved = false;
            if sub.len() >= 2 {
                for i in 0..sub.len() {
                    for j in (i + 1)..sub.len() {
                        let p = &sub[i];
                        let q = &sub[j];
                        let (lo, hi) = (
                            p.sample.min(q.sample) as usize,
                            p.sample.max(q.sample) as usize,
                        );
                        let gap = hi - lo;
                        // Two physical near lobes are separated by several
                        // samples; adjacent maxima are just envelope grain.
                        if gap < 4 || gap > 10 {
                            continue;
                        }
                        let smaller = p.amp.min(q.amp);
                        if smaller < 0.9 * env[peak] {
                            continue;
                        }
                        let valley = env[lo..=hi].iter().cloned().fold(f64::INFINITY, f64::min);
                        if smaller > 0.0 && valley / smaller >= 0.62 {
                            unresolved = true;
                        }
                    }
                }
            }
            if unresolved {
                CandidateKind::Composite
            } else {
                CandidateKind::Single
            }
        };
        regions.push(Region {
            start,
            end,
            peak,
            peak_amp: env[peak],
            saturated,
            sub,
            kind,
        });
    }
    regions
}

fn overlaps(c: &Candidate, start: usize, end: usize) -> bool {
    c.start < end as i64 && c.end > start as i64
}

fn make_candidate(
    world: &mut World,
    scan_id: i64,
    gate_kind: String,
    r: &Region,
    origin: &str,
) -> Candidate {
    let n = world.next_counter(&format!("scan{}", scan_id));
    let calib_id = world.current_calib_id;
    let depth = world.live_depth_mm(scan_id, r.peak as i64);
    Candidate {
        id: format!("s{}-c{}", scan_id, n),
        scan_id,
        gate_kind,
        start: r.start as i64,
        end: r.end as i64,
        peak: r.peak as i64,
        amplitude: r.peak_amp * crate::fixture::ADC_SCALE,
        lower_bound: r.saturated,
        kind: r.kind,
        sub_peaks: r.sub.clone(),
        verdict: None,
        track_id: None,
        origin: origin.to_string(),
        depth_snapshot_mm: depth,
        calib_id_snapshot: calib_id,
        locked: false,
    }
}

fn rerun_detection(world: &mut World) {
    // Keep locked candidates (verdicts, tracks, splits). Replace everything else.
    let kept: Vec<Candidate> = world
        .candidates
        .iter()
        .filter(|c| c.locked)
        .cloned()
        .collect();
    world.candidates = kept;

    let mut additions: Vec<Candidate> = Vec::new();
    for idx in 0..world.scans.len() {
        let scan = world.scans[idx].clone();
        let thr = threshold_value(world, scan.noise_floor);
        let regions = detect_regions(&scan.envelope, &scan.raw, thr);
        for r in regions {
            let gate = match world.gate_of(r.peak as i64) {
                Some(g) => g.clone(),
                None => continue,
            };
            if world
                .candidates
                .iter()
                .any(|c| c.scan_id == scan.id && overlaps(c, r.start, r.end))
            {
                continue;
            }
            if additions
                .iter()
                .any(|c| c.scan_id == scan.id && overlaps(c, r.start, r.end))
            {
                continue;
            }
            additions.push(make_candidate(
                world,
                scan.id,
                gate.kind.clone(),
                &r,
                "auto",
            ));
        }
    }
    world.candidates.extend(additions);
    world
        .candidates
        .sort_by_key(|c| (c.scan_id, c.peak, c.id.clone()));
    for sv in world.scans.iter_mut() {
        sv.candidates = world
            .candidates
            .iter()
            .filter(|c| c.scan_id == sv.id)
            .map(|c| c.id.clone())
            .collect();
    }
    world.check_inversion();
}

fn build_scan_views(surfaces: &BTreeMap<i64, i64>) -> Vec<ScanView> {
    let defs = scan_defs();
    let raw_scans: Vec<Scan> = scans();
    raw_scans
        .iter()
        .zip(defs.iter())
        .map(|(scan, def)| {
            let norm = crate::fixture::normalized(&scan.raw);
            let envelope = hilbert_envelope(&norm);
            let (a, b) = NOISE_WINDOW;
            let noise_floor = {
                let seg = &envelope[a..b];
                let mean: f64 = seg.iter().sum::<f64>() / seg.len() as f64;
                let var: f64 =
                    seg.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / seg.len() as f64;
                var.sqrt()
            };
            let surface = *surfaces
                .get(&scan.id)
                .unwrap_or(&(def.surface_mu.round() as i64));
            ScanView {
                id: scan.id,
                x_mm: scan.x_mm,
                y_mm: scan.y_mm,
                recorded_surface_sample: scan.recorded_surface_sample,
                surface_sample: surface,
                raw: scan.raw.clone(),
                envelope,
                noise_floor,
                candidates: Vec::new(),
            }
        })
        .collect()
}

pub fn boot(
    calib_id: i64,
    mode: &str,
    threshold_amp: f64,
    threshold_snr: f64,
) -> Result<World, String> {
    if !CALIBS.iter().any(|c| c.id == calib_id) {
        return Err(format!("unknown calibration {}", calib_id));
    }
    let mode = if mode == "snr" { "snr" } else { "amp" };
    let surfaces: BTreeMap<i64, i64> = scan_defs()
        .iter()
        .map(|d| (d.id, d.surface_mu.round() as i64))
        .collect();
    let mut world = World {
        calibs: CALIBS.to_vec(),
        current_calib_id: calib_id,
        threshold_mode: mode.to_string(),
        threshold_amp: threshold_amp.max(0.0),
        threshold_snr: threshold_snr.max(0.0),
        gates: DEFAULT_GATES
            .iter()
            .map(|(k, lo, hi)| Gate {
                kind: k.to_string(),
                lo: *lo as i64,
                hi: *hi as i64,
            })
            .collect(),
        surfaces,
        candidates: Vec::new(),
        tracks: Vec::new(),
        counters: BTreeMap::new(),
        inverted: false,
        inverted_reason: None,
        scans: Vec::new(),
    };
    world.scans = build_scan_views(&world.surfaces);
    rerun_detection(&mut world);
    Ok(world)
}

fn adjacent(world: &World, a_id: i64, b_id: i64) -> bool {
    let pa = match world.scan(a_id) {
        Some(s) => (s.x_mm, s.y_mm),
        None => return false,
    };
    let pb = match world.scan(b_id) {
        Some(s) => (s.x_mm, s.y_mm),
        None => return false,
    };
    let dx = pa.0 - pb.0;
    let dy = pa.1 - pb.1;
    (dx * dx + dy * dy).sqrt() <= 5.0 + 1e-9
}

pub fn apply(world: &mut World, event: &Event) -> Result<(), String> {
    match event {
        Event::Bootstrap { .. } => Err("bootstrap is only used to initialize an empty log".into()),
        Event::SurfaceCorrected { scan_id, sample } => {
            if *sample < 0 || *sample as usize >= crate::fixture::N {
                return Err("surface sample out of range".into());
            }
            if !world.surfaces.contains_key(scan_id) {
                return Err(format!("unknown scan {}", scan_id));
            }
            world.surfaces.insert(*scan_id, *sample);
            if let Some(sv) = world.scans.iter_mut().find(|s| s.id == *scan_id) {
                sv.surface_sample = *sample;
            }
            // Existing candidates keep their original sample interval; refresh gate assignment.
            let gates = world.gates.clone();
            for cand in world.candidates.iter_mut() {
                if let Some(g) = gates.iter().find(|g| g.contains_sample(cand.peak)) {
                    cand.gate_kind = g.kind.clone();
                }
            }
            world.check_inversion();
            Ok(())
        }
        Event::ThresholdChanged { mode, value } => {
            if *mode != "amp" && *mode != "snr" {
                return Err("threshold mode must be amp or snr".into());
            }
            if *value <= 0.0 {
                return Err("threshold must be positive".into());
            }
            world.threshold_mode = mode.clone();
            if *mode == "amp" {
                world.threshold_amp = *value;
            } else {
                world.threshold_snr = *value;
            }
            rerun_detection(world);
            Ok(())
        }
        Event::GateMoved { kind, lo, hi } => {
            if *lo < 0 || *hi as usize > crate::fixture::N || *lo >= *hi {
                return Err("gate interval must satisfy 0 <= lo < hi <= N".into());
            }
            let gate = world
                .gates
                .iter_mut()
                .find(|g| g.kind == *kind)
                .ok_or_else(|| format!("unknown gate {}", kind))?;
            gate.lo = *lo;
            gate.hi = *hi;
            let gates = world.gates.clone();
            for cand in world.candidates.iter_mut() {
                if let Some(g) = gates.iter().find(|g| g.contains_sample(cand.peak)) {
                    cand.gate_kind = g.kind.clone();
                } else {
                    cand.gate_kind = String::new();
                }
            }
            Ok(())
        }
        Event::CalibrationSwitched { calib_id } => {
            if !CALIBS.iter().any(|c| c.id == *calib_id) {
                return Err(format!("unknown calibration {}", calib_id));
            }
            world.current_calib_id = *calib_id;
            world.check_inversion();
            Ok(())
        }
        Event::CandidateVerdict {
            candidate_id,
            verdict,
        } => {
            if world.inverted {
                return Err(format!(
                    "depth axis inverted ({}); correct the surface arrival before verdicts",
                    world.inverted_reason.clone().unwrap_or_default()
                ));
            }
            let cand = world
                .candidates
                .iter_mut()
                .find(|c| &c.id == candidate_id)
                .ok_or_else(|| format!("unknown candidate {}", candidate_id))?;
            cand.verdict = Some(*verdict);
            cand.locked = true;
            world.check_inversion();
            Ok(())
        }
        Event::CandidateSplit { parent_id, cut } => {
            if world.inverted {
                return Err(
                    "depth axis inverted; correct the surface arrival before splitting".into(),
                );
            }
            let parent_pos = world
                .candidates
                .iter()
                .position(|c| &c.id == parent_id)
                .ok_or_else(|| format!("unknown candidate {}", parent_id))?;
            if world.candidates[parent_pos].kind == CandidateKind::Saturated {
                return Err(
                    "saturated platform cannot be split; amplitude remains a lower bound".into(),
                );
            }
            let parent = world.candidates[parent_pos].clone();
            if *cut <= parent.start || *cut >= parent.end {
                return Err(
                    "split cut must lie strictly inside the candidate sample interval".into(),
                );
            }
            let scan = world
                .scan(parent.scan_id)
                .ok_or_else(|| format!("unknown scan {}", parent.scan_id))?;
            let env = scan.envelope.clone();
            let raw = scan.raw.clone();
            let left = detect_regions(
                &env[parent.start as usize..*cut as usize],
                &raw[parent.start as usize..*cut as usize],
                0.0,
            );
            let right = detect_regions(
                &env[*cut as usize..parent.end as usize],
                &raw[*cut as usize..parent.end as usize],
                0.0,
            );
            if left.is_empty() || right.is_empty() {
                return Err("split would leave a side without a lobe".into());
            }
            let shift = |r: &Region, off: usize| Region {
                start: r.start + off,
                end: r.end + off,
                peak: r.peak + off,
                peak_amp: r.peak_amp,
                saturated: r.saturated,
                sub: r
                    .sub
                    .iter()
                    .map(|s| SubPeak {
                        sample: s.sample + off as i64,
                        amp: s.amp,
                    })
                    .collect(),
                kind: if r.saturated {
                    CandidateKind::Saturated
                } else {
                    CandidateKind::Single
                },
            };
            let rl = shift(&left[0], parent.start as usize);
            let rr = shift(&right[0], *cut as usize);
            let gate_kind = parent.gate_kind.clone();
            let mut left_cand =
                make_candidate(world, parent.scan_id, gate_kind.clone(), &rl, "split");
            left_cand.kind = CandidateKind::Split;
            left_cand.locked = true;
            let mut right_cand = make_candidate(world, parent.scan_id, gate_kind, &rr, "split");
            right_cand.kind = CandidateKind::Split;
            right_cand.locked = true;
            world.candidates.remove(parent_pos);
            world.candidates.push(left_cand);
            world.candidates.push(right_cand);
            world
                .candidates
                .sort_by_key(|c| (c.scan_id, c.peak, c.id.clone()));
            for sv in world.scans.iter_mut() {
                sv.candidates = world
                    .candidates
                    .iter()
                    .filter(|c| c.scan_id == sv.id)
                    .map(|c| c.id.clone())
                    .collect();
            }
            Ok(())
        }
        Event::TrackGrouped {
            track_id,
            candidate_ids,
        } => {
            if world.inverted {
                return Err(
                    "depth axis inverted; correct the surface before grouping a track".into(),
                );
            }
            if candidate_ids.len() < 2 {
                return Err("a defect track needs at least two candidates".into());
            }
            let mut scan_ids = Vec::new();
            for cid in candidate_ids {
                let cand = world
                    .candidates
                    .iter()
                    .find(|c| &c.id == cid)
                    .ok_or_else(|| format!("unknown candidate {}", cid))?;
                if cand.track_id.is_some() {
                    return Err(format!("candidate {} already belongs to a track", cid));
                }
                scan_ids.push(cand.scan_id);
            }
            let mut distinct = scan_ids.clone();
            distinct.sort();
            distinct.dedup();
            if distinct.len() != candidate_ids.len() {
                return Err(
                    "each scan position can contribute at most one candidate to a track".into(),
                );
            }
            // Order by lateral position, require an unbroken chain of adjacent positions.
            let mut ordered: Vec<(f64, f64, String)> = candidate_ids
                .iter()
                .filter_map(|cid| world.candidates.iter().find(|c| &c.id == cid))
                .map(|c| {
                    let s = world.scan(c.scan_id).unwrap();
                    (s.x_mm, s.y_mm, c.id.clone())
                })
                .collect();
            ordered.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
            for w in ordered.windows(2) {
                let sa = world
                    .scans
                    .iter()
                    .find(|s| (s.x_mm - w[0].0).abs() < 1e-9)
                    .unwrap()
                    .id;
                let sb = world
                    .scans
                    .iter()
                    .find(|s| (s.x_mm - w[1].0).abs() < 1e-9)
                    .unwrap()
                    .id;
                if !adjacent(world, sa, sb) {
                    return Err(
                        "track candidates must form a chain of adjacent scan positions".into(),
                    );
                }
            }
            for cand in world.candidates.iter_mut() {
                if candidate_ids.contains(&cand.id) {
                    cand.track_id = Some(*track_id);
                    cand.locked = true;
                }
            }
            if world.tracks.iter().any(|t| t.id == *track_id) {
                return Err(format!("track id {} already used", track_id));
            }
            world.tracks.push(Track {
                id: *track_id,
                candidate_ids: candidate_ids.clone(),
            });
            Ok(())
        }
    }
}

pub fn next_track_id(world: &World) -> i64 {
    world.tracks.iter().map(|t| t.id).max().unwrap_or(0) + 1
}
