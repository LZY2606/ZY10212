//! Deterministic signal synthesis, Hilbert envelope and candidate adjudication.

use serde::{Deserialize, Serialize};

/// In-place iterative radix-2 FFT. `n` must be a power of two.
fn fft(re: &mut [f64], im: &mut [f64], inverse: bool) {
    let n = re.len();
    debug_assert_eq!(n, im.len());
    debug_assert!(n.is_power_of_two());
    let mut j = 0usize;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j ^= bit;
        if i < j {
            re.swap(i, j);
            im.swap(i, j);
        }
    }
    let sign = if inverse { 1.0 } else { -1.0 };
    let mut len = 2usize;
    while len <= n {
        let ang = sign * 2.0 * std::f64::consts::PI / len as f64;
        let w_re = ang.cos();
        let w_im = ang.sin();
        let half = len / 2;
        let mut start = 0;
        while start < n {
            let mut cur_re = 1.0f64;
            let mut cur_im = 0.0f64;
            for k in 0..half {
                let a = start + k;
                let b = a + half;
                let tre = cur_re * re[b] - cur_im * im[b];
                let tim = cur_re * im[b] + cur_im * re[b];
                re[b] = re[a] - tre;
                im[b] = im[a] - tim;
                re[a] += tre;
                im[a] += tim;
                let nre = cur_re * w_re - cur_im * w_im;
                cur_im = cur_re * w_im + cur_im * w_re;
                cur_re = nre;
            }
            start += len;
        }
        len <<= 1;
    }
    if inverse {
        for v in re.iter_mut().chain(im.iter_mut()) {
            *v /= n as f64;
        }
    }
}

/// Magnitude envelope of `wave` via analytic signal (FFT zero-padded to a power of two).
pub fn envelope(wave: &[f64]) -> Vec<f64> {
    let n = wave.len();
    let size = n.next_power_of_two();
    let mut re = vec![0.0; size];
    let mut im = vec![0.0; size];
    re[..n].copy_from_slice(wave);
    fft(&mut re, &mut im, false);
    // Analytic-signal spectrum: keep DC, double positive frequencies, drop negatives.
    for k in 1..size {
        let pos = if k <= size / 2 { 2.0 } else { 0.0 };
        re[k] *= pos;
        im[k] *= pos;
    }
    fft(&mut re, &mut im, true);
    re[..n]
        .iter()
        .zip(&im[..n])
        .map(|(x, y)| (x * x + y * y).sqrt())
        .collect()
}

pub fn rms(samples: &[f64]) -> f64 {
    if samples.is_empty() {
        return 0.0;
    }
    (samples.iter().map(|x| x * x).sum::<f64>() / samples.len() as f64).sqrt()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThresholdMode {
    Amplitude,
    Snr,
}

impl ThresholdMode {
    pub fn as_str(self) -> &'static str {
        match self {
            ThresholdMode::Amplitude => "amplitude",
            ThresholdMode::Snr => "snr",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "amplitude" => Some(ThresholdMode::Amplitude),
            "snr" => Some(ThresholdMode::Snr),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnalConfig {
    pub velocity_m_s: f64,
    pub probe_delay_ns: f64,
    pub sample_interval_ns: f64,
    pub min_resolvable_ns: f64,
    pub threshold_mode: ThresholdMode,
    pub amp_threshold: f64,
    pub snr_threshold: f64,
    pub noise_window: [usize; 2],
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Gate {
    pub id: String,
    pub label: String,
    pub start: usize,
    pub end: usize,
    pub role: String,
}

impl Gate {
    /// Gates are left-closed / right-open: `start <= sample < end`.
    pub fn contains(&self, sample: usize) -> bool {
        sample >= self.start && sample < self.end
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct MemberPeak {
    pub peak_sample: usize,
    pub amp: f64,
    pub snr: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Candidate {
    pub key: String,
    pub gate_id: Option<String>,
    pub start_sample: usize,
    pub end_sample: usize,
    pub peak_sample: usize,
    pub amp: f64,
    pub snr: f64,
    pub saturated: bool,
    pub composite: bool,
    pub members: Vec<MemberPeak>,
    pub manual_split: bool,
    pub parent_key: Option<String>,
    pub range_mm: f64,
    pub depth_mm: Option<f64>,
    pub time_ns: f64,
    /// Depth-conversion version frozen onto this candidate (and its decision snapshot).
    pub velocity_m_s: f64,
    pub probe_delay_ns: f64,
    #[serde(default)]
    pub verdict: Option<String>,
    #[serde(default)]
    pub note: String,
    #[serde(default)]
    pub orphaned: bool,
    /// Velocity/probe-delay version captured in the stored decision snapshot
    /// (None for a live candidate without a recorded conclusion).
    #[serde(default)]
    pub snapshot_velocity_m_s: Option<f64>,
    #[serde(default)]
    pub snapshot_probe_delay_ns: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Split {
    pub parent_key: String,
    pub cuts: Vec<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Analysis {
    pub noise_rms: f64,
    pub threshold_value: f64,
    pub threshold_mode: String,
    pub surface_sample: usize,
    pub surface_time_ns: f64,
    pub error: Option<String>,
    pub candidates: Vec<Candidate>,
}

fn assign_gate(gates: &[Gate], peak: usize) -> Option<String> {
    gates
        .iter()
        .find(|g| g.contains(peak))
        .map(|g| g.id.clone())
}

fn time_ns(sample: usize, cfg: &AnalConfig) -> f64 {
    sample as f64 * cfg.sample_interval_ns
}

/// Absolute range from the probe face using the one-way instrument timeline.
fn range_mm(sample: usize, cfg: &AnalConfig) -> f64 {
    let material_ns = time_ns(sample, cfg) - cfg.probe_delay_ns;
    material_ns.max(0.0) * 1e-9 * cfg.velocity_m_s / 2.0 * 1000.0
}

fn make_candidate(
    key: String,
    start_sample: usize,
    end_sample: usize,
    peak_sample: usize,
    env: &[f64],
    wave: &[f64],
    noise_rms: f64,
    gates: &[Gate],
    cfg: &AnalConfig,
    composite: bool,
    members: Vec<MemberPeak>,
    depth: &Option<(f64, f64)>,
    manual_split: bool,
    parent_key: Option<String>,
) -> Candidate {
    let sat = wave[start_sample..=end_sample.min(wave.len() - 1)]
        .iter()
        .any(|v| v.abs() >= 1.0 - 1e-9);
    let (range_mm, depth_mm): (f64, Option<f64>) = match depth {
        Some((_, surf_range)) => {
            let r = range_mm(peak_sample, cfg);
            (r, Some(r - surf_range))
        }
        None => (range_mm(peak_sample, cfg), None),
    };
    Candidate {
        key,
        gate_id: assign_gate(gates, peak_sample),
        start_sample,
        end_sample,
        peak_sample,
        amp: env[peak_sample],
        snr: if noise_rms > 0.0 {
            env[peak_sample] / noise_rms
        } else {
            f64::NAN
        },
        saturated: sat,
        composite,
        members,
        manual_split,
        parent_key,
        range_mm,
        depth_mm,
        time_ns: time_ns(peak_sample, cfg),
        velocity_m_s: cfg.velocity_m_s,
        probe_delay_ns: cfg.probe_delay_ns,
        verdict: None,
        note: String::new(),
        orphaned: false,
        snapshot_velocity_m_s: None,
        snapshot_probe_delay_ns: None,
    }
}

impl Candidate {
    pub fn serde_attach(&mut self, verdict: String, note: String, orphaned: Option<bool>) {
        self.verdict = Some(verdict);
        self.note = note;
        if let Some(o) = orphaned {
            self.orphaned = o;
        }
    }
}

/// Split a wide (composite) candidate at integer boundaries.
/// `cuts` are the right-open inner boundaries; the parent span is reused as outer bounds.
pub fn split_candidate(
    parent: &Candidate,
    cuts: &[usize],
    env: &[f64],
    wave: &[f64],
    noise_rms: f64,
    gates: &[Gate],
    cfg: &AnalConfig,
    depth: &Option<(f64, f64)>,
) -> Vec<Candidate> {
    let mut bounds = vec![parent.start_sample];
    for c in cuts {
        if *c > parent.start_sample && *c < parent.end_sample {
            bounds.push(*c);
        }
    }
    bounds.sort_unstable();
    bounds.dedup();
    bounds.push(parent.end_sample);

    let mut out = Vec::new();
    for (i, w) in bounds.windows(2).enumerate() {
        let (s, e) = (w[0], w[1]);
        if e <= s {
            continue;
        }
        let peak = (s..e)
            .max_by(|a, b| env[*a].total_cmp(&env[*b]))
            .unwrap_or(s);
        let key = format!("{}#s{}", parent.key, i + 1);
        out.push(make_candidate(
            key,
            s,
            e,
            peak,
            env,
            wave,
            noise_rms,
            gates,
            cfg,
            false,
            Vec::new(),
            depth,
            true,
            Some(parent.key.clone()),
        ));
    }
    out
}

/// Run the deterministic adjudication analysis for one position.
///
/// * `wave`    - raw time-domain samples (already clipped to [-1, 1] on synthesis).
/// * `splits`  - manual splits recorded for this position.
///
/// Returns `error = "surface_after_bottom"` (and no depth values) when the surface
/// arrival is later than the bottom candidate; the depth axis must never be inverted.
pub fn analyze(
    wave: &[f64],
    gates: &[Gate],
    cfg: &AnalConfig,
    surface_sample: usize,
    splits: &[Split],
) -> Analysis {
    let env = envelope(wave);
    // Detection envelope: short moving average suppresses residual carrier ripple
    // (~one carrier period) so ripple maxima are not mistaken for separate echoes.
    // ~half a carrier period (carrier is 5 MHz @ 10 ns => 20 samples/period),
    // wide enough to suppress carrier ripple on the detection envelope while
    // preserving two echoes separated by the Rayleigh-style resolution limit.
    let half_win = 9usize;
    let mut prefix = vec![0.0f64; env.len() + 1];
    for (i, v) in env.iter().enumerate() {
        prefix[i + 1] = prefix[i] + v;
    }
    let smooth: Vec<f64> = (0..env.len())
        .map(|i| {
            let lo = i.saturating_sub(half_win);
            let hi = (i + half_win + 1).min(env.len());
            (prefix[hi] - prefix[lo]) / (hi - lo) as f64
        })
        .collect();
    let (nw0, nw1) = (
        cfg.noise_window[0].min(wave.len()),
        cfg.noise_window[1].min(wave.len()),
    );
    let noise_rms = if nw1 > nw0 { rms(&wave[nw0..nw1]) } else { 0.0 };
    let threshold_value = match cfg.threshold_mode {
        ThresholdMode::Amplitude => cfg.amp_threshold,
        ThresholdMode::Snr => cfg.snr_threshold * noise_rms,
    };

    // Raw envelope local maxima (carry both genuine close echoes and carrier ripple).
    let mut raw_maxima: Vec<usize> = Vec::new();
    for i in 1..env.len() - 1 {
        if env[i] >= threshold_value && env[i] >= env[i - 1] && env[i] > env[i + 1] {
            raw_maxima.push(i);
        }
    }

    // Smooth-envelope contiguous above-threshold regions bound every candidate.
    let mut regions: Vec<(usize, usize, Vec<usize>)> = Vec::new();
    let mut k = 0usize;
    while k < env.len() {
        if smooth[k] < threshold_value {
            k += 1;
            continue;
        }
        let start = k;
        while k < smooth.len() && smooth[k] >= threshold_value {
            k += 1;
        }
        let end = k; // exclusive
        let peaks: Vec<usize> = raw_maxima
            .iter()
            .copied()
            .filter(|m| *m >= start && *m < end)
            .collect();
        regions.push((start, end, peaks));
    }

    let min_sep_samples = (cfg.min_resolvable_ns / cfg.sample_interval_ns)
        .round()
        .max(1.0) as usize;

    // Prominent peak fraction: carrier-ripple lobes stay within ~15% of the region
    // maximum, while genuine unresolved echoes form a valley well below that.
    const PROMINENCE: f64 = 0.45;
    // Two peaks are only treated as distinct when the valley between them drops
    // below this fraction of the lower peak (a resolvable dip).
    const VALLEY: f64 = 0.85;

    // Within each region find prominent maxima, then merge pairs that cannot be
    // resolved (closer than the limit and without a deep enough valley), and never
    // split a saturated plateau.
    let mut raw: Vec<Candidate> = Vec::new();
    let surf_range = range_mm(surface_sample, cfg);

    for (idx, (rstart, rend, peaks)) in regions.into_iter().enumerate() {
        if peaks.is_empty() {
            continue;
        }
        let region_max = peaks.iter().map(|m| env[*m]).fold(0.0f64, f64::max);
        let prominent: Vec<usize> = peaks
            .iter()
            .copied()
            .filter(|m| env[*m] >= PROMINENCE * region_max)
            .collect();
        if prominent.is_empty() {
            continue;
        }

        // Saturated region: clipping flattens the envelope so peaks cannot be
        // resolved; emit a single candidate (members empty, never force-split).
        let region_saturated = wave[rstart..rend].iter().any(|v| v.abs() >= 1.0 - 1e-9);

        // Pick genuine echo lobes among the prominent maxima: walk outward from
        // the global maximum and accept another lobe only if the valley between
        // them dips below VALLEY of the lower peak. Carrier ripple stays on the
        // shoulder of one lobe (valley > 0.95) and is therefore ignored.
        let anchor = *prominent
            .iter()
            .max_by(|a, b| env[**a].total_cmp(&env[**b]))
            .unwrap();
        let mut lobes: Vec<usize> = vec![anchor];
        for &p in prominent.iter() {
            if p == anchor {
                continue;
            }
            let nearest = lobes
                .iter()
                .copied()
                .min_by_key(|l| (*l as isize - p as isize).unsigned_abs())
                .unwrap();
            let (a, b) = (nearest.min(p), nearest.max(p));
            let valley = env[a..=b].iter().cloned().fold(f64::INFINITY, f64::min);
            let lower = env[a].min(env[b]);
            if valley < VALLEY * lower {
                lobes.push(p);
            }
        }
        lobes.sort_unstable();

        let mut clusters: Vec<Vec<usize>> = Vec::new();
        if region_saturated || lobes.len() < 2 {
            // Single echo (or saturated plateau): one candidate, no composite.
            clusters.push(vec![anchor]);
        } else {
            // Group genuinely distinct lobes that are closer than the resolution
            // limit into one unresolved composite candidate.
            let mut current = vec![lobes[0]];
            for &p in lobes.iter().skip(1) {
                let last = *current.last().unwrap();
                if p - last < min_sep_samples {
                    current.push(p);
                } else {
                    clusters.push(std::mem::take(&mut current));
                    current = vec![p];
                }
            }
            clusters.push(current);
        }
        for (cidx, cluster) in clusters.into_iter().enumerate() {
            let peak = *cluster
                .iter()
                .max_by(|a, b| env[**a].total_cmp(&env[**b]))
                .unwrap();
            let composite = cluster.len() > 1;
            let members = cluster
                .iter()
                .map(|m| MemberPeak {
                    peak_sample: *m,
                    amp: env[*m],
                    snr: if noise_rms > 0.0 {
                        env[*m] / noise_rms
                    } else {
                        f64::NAN
                    },
                })
                .collect();
            let key = format!("r{idx}c{cidx}");
            raw.push(make_candidate(
                key,
                rstart,
                rend,
                peak,
                &env,
                wave,
                noise_rms,
                gates,
                cfg,
                composite,
                members,
                &Some((surface_sample as f64, surf_range)),
                false,
                None,
            ));
        }
    }

    // Surface-after-bottom guard: no inverted depth axis, no depth results.
    let bottom_gate = gates.iter().find(|g| g.role == "bottom");
    let bottom_peak = raw
        .iter()
        .filter(|c| {
            bottom_gate
                .map(|g| g.contains(c.peak_sample))
                .unwrap_or(false)
        })
        .map(|c| c.peak_sample)
        .max();
    let error = match bottom_peak {
        Some(bp) if surface_sample > bp => Some("surface_after_bottom".to_string()),
        _ => None,
    };
    let depth_ok = error.is_none();
    let depth = if depth_ok {
        Some((surface_sample as f64, surf_range))
    } else {
        None
    };
    for c in raw.iter_mut() {
        c.depth_mm = None;
        if depth_ok {
            c.depth_mm = Some(range_mm(c.peak_sample, cfg) - surf_range);
        }
    }

    // Apply recorded manual splits to still-present wide candidates.
    let by_key: std::collections::HashMap<String, Candidate> =
        raw.iter().map(|c| (c.key.clone(), c.clone())).collect();
    let mut finalists: Vec<Candidate> = Vec::new();
    let mut split_parents: std::collections::HashSet<String> = std::collections::HashSet::new();
    for sp in splits {
        if let Some(parent) = by_key.get(&sp.parent_key) {
            split_parents.insert(sp.parent_key.clone());
            finalists.extend(split_candidate(
                parent, &sp.cuts, &env, wave, noise_rms, gates, cfg, &depth,
            ));
        }
    }
    for c in raw {
        if !split_parents.contains(&c.key) {
            finalists.push(c);
        }
    }
    finalists.sort_by_key(|c| c.peak_sample);

    Analysis {
        noise_rms,
        threshold_value,
        threshold_mode: cfg.threshold_mode.as_str().to_string(),
        surface_sample,
        surface_time_ns: time_ns(surface_sample, cfg),
        error,
        candidates: finalists,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> AnalConfig {
        AnalConfig {
            velocity_m_s: 5900.0,
            probe_delay_ns: 2000.0,
            sample_interval_ns: 10.0,
            min_resolvable_ns: 200.0,
            threshold_mode: ThresholdMode::Amplitude,
            amp_threshold: 0.15,
            snr_threshold: 4.0,
            noise_window: [720, 800],
        }
    }

    fn gates() -> Vec<Gate> {
        vec![Gate {
            id: "g".into(),
            label: "g".into(),
            start: 0,
            end: 1024,
            role: "inspection".into(),
        }]
    }

    #[test]
    fn envelope_recovers_constant_amplitude() {
        // A windowed tone burst should have an envelope tracking its Gaussian shape.
        let mut wave = vec![0.0f64; 1024];
        let echo = crate::fixture::Echo {
            sample: 512,
            fwhm_samples: 14.0,
            amp: 0.8,
            carrier_period_samples: 20.0,
            phase: 0.0,
        };
        crate::fixture::render_echo(&mut wave, &echo);
        let env = envelope(&wave);
        let peak = env.iter().cloned().fold(0.0f64, f64::max);
        assert!((peak - 0.8).abs() < 0.02, "peak envelope {peak}");
        // Away from the burst the envelope must decay to the noise floor (~0).
        assert!(env[0] < 1e-3 && env[1023] < 1e-2);
    }

    #[test]
    fn two_unresolved_peaks_keep_composite() {
        let mut wave = vec![0.0f64; 1024];
        let e1 = crate::fixture::Echo {
            sample: 400,
            fwhm_samples: 12.0,
            amp: 0.34,
            carrier_period_samples: 20.0,
            phase: 0.0,
        };
        let e2 = crate::fixture::Echo {
            sample: 416,
            fwhm_samples: 12.0,
            amp: 0.31,
            carrier_period_samples: 20.0,
            phase: 0.5,
        };
        crate::fixture::render_echo(&mut wave, &e1);
        crate::fixture::render_echo(&mut wave, &e2);
        let a = analyze(&wave, &gates(), &cfg(), 300, &[]);
        let comp: Vec<&Candidate> = a.candidates.iter().filter(|c| c.composite).collect();
        assert!(!comp.is_empty(), "expected a composite candidate");
        assert!(comp[0].members.len() >= 2);
    }

    #[test]
    fn split_produces_two_children() {
        let mut wave = vec![0.0f64; 1024];
        let e1 = crate::fixture::Echo {
            sample: 400,
            fwhm_samples: 12.0,
            amp: 0.4,
            carrier_period_samples: 20.0,
            phase: 0.0,
        };
        let e2 = crate::fixture::Echo {
            sample: 416,
            fwhm_samples: 12.0,
            amp: 0.4,
            carrier_period_samples: 20.0,
            phase: 0.5,
        };
        crate::fixture::render_echo(&mut wave, &e1);
        crate::fixture::render_echo(&mut wave, &e2);
        let a = analyze(&wave, &gates(), &cfg(), 300, &[]);
        let parent = a.candidates.iter().find(|c| c.composite).unwrap().clone();
        let children = split_candidate(
            &parent,
            &[408],
            &envelope(&wave),
            &wave,
            a.noise_rms,
            &gates(),
            &cfg(),
            &Some((300.0, 0.0)),
        );
        assert_eq!(children.len(), 2);
        assert!(children.iter().all(|c| c.manual_split));
    }

    #[test]
    fn surface_after_bottom_flags_error_and_no_depth() {
        let mut wave = vec![0.0f64; 1024];
        let surf = crate::fixture::Echo {
            sample: 580,
            fwhm_samples: 20.0,
            amp: 0.8,
            carrier_period_samples: 20.0,
            phase: 0.0,
        };
        let bot = crate::fixture::Echo {
            sample: 560,
            fwhm_samples: 20.0,
            amp: 0.55,
            carrier_period_samples: 20.0,
            phase: 0.0,
        };
        crate::fixture::render_echo(&mut wave, &surf);
        crate::fixture::render_echo(&mut wave, &bot);
        let g = vec![
            Gate {
                id: "surface".into(),
                label: "s".into(),
                start: 576,
                end: 620,
                role: "surface".into(),
            },
            Gate {
                id: "bottom".into(),
                label: "b".into(),
                start: 540,
                end: 576,
                role: "bottom".into(),
            },
        ];
        let a = analyze(&wave, &g, &cfg(), 590, &[]);
        assert_eq!(a.error.as_deref(), Some("surface_after_bottom"));
        assert!(a.candidates.iter().all(|c| c.depth_mm.is_none()));
    }
}
