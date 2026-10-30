//! Deterministic test signals (sines, white and pink noise, click trains) for tests, benches and downstream fixtures.

/// xorshift64* PRNG: deterministic and dependency-free.
#[derive(Clone, Debug)]
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }

    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// Uniform in [-1, 1).
    pub fn uniform(&mut self) -> f32 {
        ((self.next_u64() >> 40) as f32 / (1u64 << 24) as f32) * 2.0 - 1.0
    }
}

/// Sine of `amp` peak amplitude.
pub fn sine(freq_hz: f32, amp: f32, n: usize, sample_rate: u32, phase: f32) -> Vec<f32> {
    let w = std::f64::consts::TAU * freq_hz as f64 / sample_rate as f64;
    (0..n).map(|i| amp * (w * i as f64 + phase as f64).sin() as f32).collect()
}

/// Uniform white noise scaled to `rms`.
pub fn white(n: usize, rms: f32, seed: u64) -> Vec<f32> {
    let mut r = Rng::new(seed);
    let mut v: Vec<f32> = (0..n).map(|_| r.uniform()).collect();
    scale_to_rms(&mut v, rms);
    v
}

/// Pink (−3 dB/octave) noise via Paul Kellet's refined filter, scaled to `rms`.
pub fn pink(n: usize, rms: f32, seed: u64) -> Vec<f32> {
    let mut r = Rng::new(seed);
    let mut b = [0.0f32; 7];
    let mut v = Vec::with_capacity(n);
    for _ in 0..n {
        let w = r.uniform();
        b[0] = 0.99886 * b[0] + w * 0.0555179;
        b[1] = 0.99332 * b[1] + w * 0.0750759;
        b[2] = 0.96900 * b[2] + w * 0.1538520;
        b[3] = 0.86650 * b[3] + w * 0.3104856;
        b[4] = 0.55000 * b[4] + w * 0.5329522;
        b[5] = -0.7616 * b[5] - w * 0.0168980;
        let out = b[0] + b[1] + b[2] + b[3] + b[4] + b[5] + b[6] + w * 0.5362;
        b[6] = w * 0.115926;
        v.push(out);
    }
    scale_to_rms(&mut v, rms);
    v
}

/// Exponentially decaying noise bursts (`rate_hz` per second, 30 ms decay) over a quiet noise floor (−60 dB).
pub fn click_train(rate_hz: f32, amp: f32, n: usize, sample_rate: u32, seed: u64) -> Vec<f32> {
    let mut r = Rng::new(seed);
    let period = (sample_rate as f32 / rate_hz) as usize;
    let decay = (-1.0 / (0.03 * sample_rate as f32)).exp();
    let mut env = 0.0f32;
    (0..n)
        .map(|i| {
            if i % period == period / 2 {
                env = 1.0;
            }
            let v = amp * env * r.uniform() + amp * 1e-3 * r.uniform();
            env *= decay;
            v
        })
        .collect()
}

/// Root mean square of `v`.
pub fn rms(v: &[f32]) -> f32 {
    if v.is_empty() {
        return 0.0;
    }
    (v.iter().map(|x| (*x as f64) * (*x as f64)).sum::<f64>() / v.len() as f64).sqrt() as f32
}

/// Scales `v` in place to the given RMS.
pub fn scale_to_rms(v: &mut [f32], target: f32) {
    let r = rms(v);
    if r > 0.0 {
        let g = target / r;
        v.iter_mut().for_each(|x| *x *= g);
    }
}

/// Interleaves two equal-length channels.
pub fn interleave(l: &[f32], r: &[f32]) -> Vec<f32> {
    l.iter().zip(r).flat_map(|(a, b)| [*a, *b]).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levels() {
        assert!((rms(&white(48000, 0.1, 1)) - 0.1).abs() < 1e-4);
        assert!((rms(&pink(48000, 0.1, 1)) - 0.1).abs() < 1e-4);
        assert!((rms(&sine(1000.0, 1.0, 48000, 48000, 0.0)) - std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-3);
        assert_eq!(interleave(&[1.0, 2.0], &[3.0, 4.0]), vec![1.0, 3.0, 2.0, 4.0]);
    }
}
