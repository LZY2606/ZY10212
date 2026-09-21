//! Fixed fixture parsing and deterministic waveform synthesis.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Echo {
    pub sample: usize,
    pub fwhm_samples: f64,
    pub amp: f64,
    pub carrier_period_samples: f64,
    #[serde(default)]
    pub phase: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PositionSpec {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub echoes: Vec<Echo>,
    #[serde(default)]
    pub surface_sample: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GateSpec {
    pub id: String,
    pub label: String,
    pub start: usize,
    pub end: usize,
    pub role: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Defaults {
    pub surface_sample: usize,
    pub surface: Echo,
    pub bottom: Echo,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Noise {
    pub rms: f64,
    pub seed: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Grid {
    pub rows: usize,
    pub cols: usize,
    pub dx_mm: f64,
    pub dy_mm: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Fixture {
    pub schema: String,
    pub name: String,
    pub sample_count: usize,
    pub sample_interval_ns: f64,
    pub velocity_m_s: f64,
    pub probe_delay_ns: f64,
    pub min_resolvable_ns: f64,
    pub noise_window: [usize; 2],
    pub defaults: Defaults,
    pub noise: Noise,
    pub grid: Grid,
    pub gates: Vec<GateSpec>,
    pub positions: Vec<PositionSpec>,
}

impl Fixture {
    pub fn parse(json: &str) -> Result<Fixture, String> {
        let fx: Fixture = serde_json::from_str(json).map_err(|e| e.to_string())?;
        fx.validate()?;
        Ok(fx)
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.schema != "echo-bench-fixture/1" {
            return Err("unsupported fixture schema".to_string());
        }
        if self.sample_count == 0 || !self.sample_count.is_power_of_two() {
            return Err("sample_count must be a positive power of two".to_string());
        }
        if self.sample_interval_ns <= 0.0
            || self.velocity_m_s <= 0.0
            || self.min_resolvable_ns <= 0.0
        {
            return Err("interval, velocity and resolution must be positive".to_string());
        }
        if self.noise.rms < 0.0 {
            return Err("noise rms must be non-negative".to_string());
        }
        let (w0, w1) = (self.noise_window[0], self.noise_window[1]);
        if w1 <= w0 || w1 > self.sample_count {
            return Err("noise_window must be ordered and inside the record".to_string());
        }
        if self.gates.is_empty() {
            return Err("at least one gate is required".to_string());
        }
        for g in &self.gates {
            if g.end <= g.start || g.end > self.sample_count {
                return Err(format!("gate {} has invalid bounds", g.id));
            }
        }
        let mut sorted: Vec<&GateSpec> = self.gates.iter().collect();
        sorted.sort_by_key(|g| g.start);
        for w in sorted.windows(2) {
            if w[0].end > w[1].start {
                return Err(format!("gates {} and {} overlap", w[0].id, w[1].id));
            }
        }
        if !sorted.iter().any(|g| g.role == "surface") {
            return Err("a surface gate is required".to_string());
        }
        if !sorted.iter().any(|g| g.role == "bottom") {
            return Err("a bottom gate is required".to_string());
        }
        if self.grid.rows * self.grid.cols != self.positions.len() {
            return Err("grid size must match the number of positions".to_string());
        }
        Ok(())
    }

    pub fn surface_sample_for(&self, p: &PositionSpec) -> usize {
        p.surface_sample.unwrap_or(self.defaults.surface_sample)
    }
}

/// Deterministic xorshift64* PRNG so fixtures replay identically everywhere.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Rng(seed.max(1))
    }
    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn next_f64(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }
    fn normal(&mut self) -> f64 {
        // Box-Muller
        let u1 = self.next_f64().max(1e-12);
        let u2 = self.next_f64();
        (-2.0 * u1.ln()).sqrt() * (2.0 * std::f64::consts::PI * u2).cos()
    }
}

pub fn render_echo(buf: &mut [f64], e: &Echo) {
    let sigma = e.fwhm_samples / (2.0 * (2.0_f64.ln()).sqrt());
    let half = (3.0 * sigma).ceil() as isize;
    let center = e.sample as isize;
    for i in (center - half)..=(center + half) {
        if i < 0 || i >= buf.len() as isize {
            continue;
        }
        let d = i as f64 - e.sample as f64;
        let gauss = (-(d * d) / (2.0 * sigma * sigma)).exp();
        let carrier = (2.0 * std::f64::consts::PI * d / e.carrier_period_samples + e.phase).cos();
        buf[i as usize] += e.amp * gauss * carrier;
    }
}

/// Synthesize the raw waveform for `position_index` (deterministic, fixed seed).
pub fn synthesize(fx: &Fixture, position_index: usize) -> Vec<f64> {
    let pos = &fx.positions[position_index];
    let mut wave = vec![0.0f64; fx.sample_count];
    // Position-independent noise floor keeps cross-position comparison stable.
    let mut rng = Rng::new(fx.noise.seed);
    for v in wave.iter_mut() {
        *v = fx.noise.rms * rng.normal();
    }
    render_echo(&mut wave, &fx.defaults.surface);
    render_echo(&mut wave, &fx.defaults.bottom);
    for e in &pos.echoes {
        render_echo(&mut wave, e);
    }
    // Instrument clips at full scale; clipped samples are saturated.
    for v in wave.iter_mut() {
        if *v > 1.0 {
            *v = 1.0;
        } else if *v < -1.0 {
            *v = -1.0;
        }
    }
    wave
}
