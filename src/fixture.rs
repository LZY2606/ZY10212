//! Fixed, deterministic A-scan fixture (also exported as `fixture.json`).
//!
//! Physical layout (5 lateral positions over a 20 mm steel plate):
//!   - excitation pulse around sample 50 (outside every analysis gate)
//!   - surface echo around sample 200 (sub-sample alignment jitter)
//!   - a defect at 12 mm depth around sample 608 (surface + ~407):
//!       * clean scatter at positions 1 and 2
//!       * two unresolved near peaks at position 3 (composite candidate)
//!       * one wide echo at position 4 (operator can split it)
//!       * saturated platform at position 5 (amplitude is a lower bound)
//!   - bottom echo (20 mm) around sample 879

use serde::Serialize;

pub const N: usize = 2048;
pub const FS_MHZ: f64 = 100.0;
pub const ADC_MAX: i16 = 32_767;
pub const ADC_SCALE: f64 = 30_000.0;
pub const F0_MHZ: f64 = 5.0;
pub const NOISE_RMS: f64 = 0.012;

/// Noise floor estimation window (samples), quiet region before the surface.
pub const NOISE_WINDOW: (usize, usize) = (90, 150);

/// Gate templates are in samples, left-closed / right-open.
pub const DEFAULT_GATES: &[(&str, usize, usize)] = &[
    ("surface", 150, 300),
    ("inspection", 350, 660),
    ("bottom", 820, 960),
];

pub const DEFAULT_THRESHOLD_AMP: f64 = 6_000.0;
pub const DEFAULT_THRESHOLD_SNR: f64 = 6.0;

#[derive(Clone, Copy, Serialize)]
pub struct Calib {
    pub id: i64,
    pub name: &'static str,
    pub velocity_ms: f64,
    /// Probe delay, in microseconds between fire and time zero.
    pub probe_delay_us: f64,
}

pub const CALIBS: &[Calib] = &[
    Calib {
        id: 1,
        name: "steel-5900",
        velocity_ms: 5900.0,
        probe_delay_us: 0.20,
    },
    Calib {
        id: 2,
        name: "steel-5920",
        velocity_ms: 5920.0,
        probe_delay_us: 0.18,
    },
];

#[derive(Clone)]
pub struct Component {
    pub mu: f64,
    pub sigma: f64,
    pub amp: f64,
    pub phase: f64,
}

#[derive(Clone)]
pub struct ScanDef {
    pub id: i64,
    pub x_mm: f64,
    pub y_mm: f64,
    pub surface_mu: f64,
    pub components: Vec<Component>,
}

#[derive(Clone, Serialize)]
pub struct Scan {
    pub id: i64,
    pub x_mm: f64,
    pub y_mm: f64,
    /// Recorded (nominal + jitter) surface arrival, in samples.
    pub recorded_surface_sample: i64,
    pub raw: Vec<i16>,
}

fn rng_state(seed: u64) -> u64 {
    seed.max(1)
}

fn next_rng(state: &mut u64) -> f64 {
    // xorshift64*, deterministic and independent of platform.
    let mut x = *state;
    x ^= x >> 12;
    x ^= x << 25;
    x ^= x >> 27;
    *state = x;
    let v = x.wrapping_mul(0x2545_F491_4F6C_DD1D);
    (v >> 11) as f64 / (1u64 << 53) as f64
}

fn gaussian(state: &mut u64) -> f64 {
    // Box-Muller.
    let u1 = next_rng(state).max(1e-12);
    let u2 = next_rng(state);
    (-2.0 * u1.ln()).sqrt() * (2.0 * std::f64::consts::PI * u2).cos()
}

fn wavelet(buf: &mut [f64], c: &Component) {
    for (n, v) in buf.iter_mut().enumerate() {
        let dt = n as f64 - c.mu;
        let env = (-0.5 * (dt / c.sigma).powi(2)).exp();
        *v += c.amp * env * (2.0 * std::f64::consts::PI * F0_MHZ * dt / FS_MHZ + c.phase).cos();
    }
}

fn build_scan(def: &ScanDef) -> Scan {
    let mut buf = vec![0.0f64; N];
    for comp in &def.components {
        wavelet(&mut buf, comp);
    }
    let mut state = rng_state(0x9E37_79B9_7F4A_7C15u64 ^ (def.id as u64 * 0x9E37_79B1));
    for v in &mut buf {
        *v += NOISE_RMS * gaussian(&mut state);
    }
    let raw: Vec<i16> = buf
        .iter()
        .map(|v| {
            let s = (v * ADC_SCALE).round();
            if s >= ADC_MAX as f64 {
                ADC_MAX
            } else if s <= -ADC_MAX as f64 {
                -ADC_MAX
            } else {
                s as i16
            }
        })
        .collect();
    Scan {
        id: def.id,
        x_mm: def.x_mm,
        y_mm: def.y_mm,
        recorded_surface_sample: def.surface_mu.round() as i64,
        raw,
    }
}

pub fn scan_defs() -> Vec<ScanDef> {
    defs_inner()
}

fn defs_inner() -> Vec<ScanDef> {
    let defs = vec![
        ScanDef {
            id: 1,
            x_mm: 0.0,
            y_mm: 0.0,
            surface_mu: 201.0,
            components: vec![
                Component {
                    mu: 50.0,
                    sigma: 3.0,
                    amp: 0.70,
                    phase: 0.0,
                },
                Component {
                    mu: 201.0,
                    sigma: 4.0,
                    amp: 0.90,
                    phase: 0.0,
                },
                Component {
                    mu: 608.0,
                    sigma: 4.0,
                    amp: 0.35,
                    phase: 0.0,
                },
                Component {
                    mu: 879.0,
                    sigma: 4.0,
                    amp: 0.50,
                    phase: 0.0,
                },
            ],
        },
        ScanDef {
            id: 2,
            x_mm: 5.0,
            y_mm: 0.0,
            surface_mu: 198.0,
            components: vec![
                Component {
                    mu: 50.0,
                    sigma: 3.0,
                    amp: 0.70,
                    phase: 0.0,
                },
                Component {
                    mu: 198.0,
                    sigma: 4.0,
                    amp: 0.90,
                    phase: 0.0,
                },
                Component {
                    mu: 608.0,
                    sigma: 4.0,
                    amp: 0.35,
                    phase: 0.3,
                },
                Component {
                    mu: 879.0,
                    sigma: 4.0,
                    amp: 0.50,
                    phase: 0.0,
                },
            ],
        },
        ScanDef {
            id: 3,
            x_mm: 10.0,
            y_mm: 0.0,
            surface_mu: 200.0,
            components: vec![
                Component {
                    mu: 50.0,
                    sigma: 3.0,
                    amp: 0.70,
                    phase: 0.0,
                },
                Component {
                    mu: 200.0,
                    sigma: 4.0,
                    amp: 0.90,
                    phase: 0.0,
                },
                // Two lobes 8 samples apart, opposing phase: one envelope blob.
                Component {
                    mu: 604.0,
                    sigma: 4.0,
                    amp: 0.30,
                    phase: 0.0,
                },
                Component {
                    mu: 612.0,
                    sigma: 4.0,
                    amp: 0.30,
                    phase: 2.4,
                },
                Component {
                    mu: 879.0,
                    sigma: 4.0,
                    amp: 0.50,
                    phase: 0.0,
                },
            ],
        },
        ScanDef {
            id: 4,
            x_mm: 15.0,
            y_mm: 0.0,
            surface_mu: 202.0,
            components: vec![
                Component {
                    mu: 50.0,
                    sigma: 3.0,
                    amp: 0.70,
                    phase: 0.0,
                },
                Component {
                    mu: 202.0,
                    sigma: 4.0,
                    amp: 0.90,
                    phase: 0.0,
                },
                // One genuinely wide echo: single broad lobe, manually splittable.
                Component {
                    mu: 608.0,
                    sigma: 7.0,
                    amp: 0.42,
                    phase: 0.0,
                },
                Component {
                    mu: 879.0,
                    sigma: 4.0,
                    amp: 0.50,
                    phase: 0.0,
                },
            ],
        },
        ScanDef {
            id: 5,
            x_mm: 20.0,
            y_mm: 0.0,
            surface_mu: 199.0,
            components: vec![
                Component {
                    mu: 50.0,
                    sigma: 3.0,
                    amp: 0.70,
                    phase: 0.0,
                },
                Component {
                    mu: 199.0,
                    sigma: 4.0,
                    amp: 0.90,
                    phase: 0.0,
                },
                // Driving amplitude beyond ADC range: saturated platform.
                Component {
                    mu: 608.0,
                    sigma: 4.0,
                    amp: 2.00,
                    phase: 0.0,
                },
                Component {
                    mu: 879.0,
                    sigma: 4.0,
                    amp: 0.50,
                    phase: 0.0,
                },
            ],
        },
    ];
    defs
}

pub fn scans() -> Vec<Scan> {
    defs_inner().iter().map(build_scan).collect()
}

/// Normalized waveform (ADC counts / ADC_SCALE) for DSP.
pub fn normalized(raw: &[i16]) -> Vec<f64> {
    raw.iter().map(|v| *v as f64 / ADC_SCALE).collect()
}
