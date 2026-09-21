//! Minimal deterministic DSP helpers: radix-2 FFT, Hilbert envelope and
//! waveform statistics used by the adjudication detector.

#[derive(Clone, Copy)]
pub struct Complex {
    pub re: f64,
    pub im: f64,
}

impl Complex {
    fn new(re: f64, im: f64) -> Self {
        Self { re, im }
    }
}

/// In-place iterative Cooley-Tukey FFT for power-of-two lengths.
/// `sign < 0` is the conventional forward transform.
pub fn fft(a: &mut [Complex], sign: f64) {
    let n = a.len();
    if n <= 1 {
        return;
    }
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
            a.swap(i, j);
        }
    }

    let mut len = 2usize;
    while len <= n {
        let ang = sign * 2.0 * std::f64::consts::PI / len as f64;
        let wlen = Complex::new(ang.cos(), ang.sin());
        let half = len / 2;
        let mut i = 0;
        while i < n {
            let mut w = Complex::new(1.0, 0.0);
            for k in 0..half {
                let u = a[i + k];
                let v = Complex::new(
                    a[i + k + half].re * w.re - a[i + k + half].im * w.im,
                    a[i + k + half].re * w.im + a[i + k + half].im * w.re,
                );
                a[i + k] = Complex::new(u.re + v.re, u.im + v.im);
                a[i + k + half] = Complex::new(u.re - v.re, u.im - v.im);
                w = Complex::new(
                    w.re * wlen.re - w.im * wlen.im,
                    w.re * wlen.im + w.im * wlen.re,
                );
            }
            i += len;
        }
        len <<= 1;
    }
}

/// Magnitude of the analytic signal (Hilbert transform envelope). Input length
/// must be a power of two; the output has the same length.
pub fn hilbert_envelope(x: &[f64]) -> Vec<f64> {
    let n = x.len();
    assert!(
        n.is_power_of_two(),
        "waveform length must be a power of two"
    );
    let mut buf: Vec<Complex> = x.iter().map(|v| Complex::new(*v, 0.0)).collect();
    fft(&mut buf, -1.0);
    for (k, c) in buf.iter_mut().enumerate() {
        let h = if k == 0 || k == n / 2 {
            1.0
        } else if k < n / 2 {
            2.0
        } else {
            0.0
        };
        c.re *= h;
        c.im *= h;
    }
    fft(&mut buf, 1.0);
    buf.iter()
        .map(|c| (c.re * c.re + c.im * c.im).sqrt() / n as f64)
        .collect()
}

/// Strict local maximum with deterministic plateau handling: a plateau's
/// middle sample is reported as the single representative maximum.
pub fn local_maxima(e: &[f64]) -> Vec<usize> {
    let mut out = Vec::new();
    let n = e.len();
    let mut i = 1;
    while i + 1 < n {
        if e[i] < e[i - 1] {
            i += 1;
            continue;
        }
        let mut j = i;
        while j + 1 < n && e[j + 1] == e[j] {
            j += 1;
        }
        if j + 1 < n && e[i] > e[i - 1] && e[j] > e[j + 1] {
            out.push((i + j) / 2);
        }
        i = j + 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envelope_tracks_gaussian_tone_burst() {
        let n = 256;
        let fs = 50.0;
        let f0 = 5.0;
        let center = 128usize;
        let sigma = 3.0f64;
        let x: Vec<f64> = (0..n)
            .map(|k| {
                let dt = (k as i64 - center as i64) as f64;
                let g = (-0.5 * (dt / sigma).powi(2)).exp();
                g * (2.0 * std::f64::consts::PI * f0 * dt / fs).cos()
            })
            .collect();
        let env = hilbert_envelope(&x);
        assert!(env[center] > 0.95, "env peak {}", env[center]);
        assert!(env[10] < 0.05);
    }

    #[test]
    fn maxima_handles_plateau() {
        let e = vec![0.0, 1.0, 2.0, 2.0, 2.0, 1.0, 0.0];
        assert_eq!(local_maxima(&e), vec![3]);
    }
}
