mod common;
use common::*;
use mix_reference::{decode_bytes, resample};

fn check_exact(d: &mix_reference::DecodedAudio, src: &[f32], ch: usize, sr: u32, tol: f32) {
    assert_eq!(d.sample_rate, sr);
    assert_eq!(d.channels, ch);
    assert_eq!(d.samples.len(), src.len());
    let worst = d
        .samples
        .iter()
        .zip(src)
        .map(|(a, b)| (a - b).abs())
        .fold(0.0f32, f32::max);
    assert!(worst <= tol, "max sample error {worst} > {tol}");
}

fn stereo_test_signal(frames: usize, sr: u32) -> Vec<f32> {
    let l = sine(440.0, 0.5, frames, sr);
    let r = sine(1000.0, 0.25, frames, sr);
    l.iter().zip(&r).flat_map(|(a, b)| [*a, *b]).collect()
}

#[test]
fn wav_16_24_float_stereo_and_mono() {
    let st = stereo_test_signal(8000, 44100);
    let q = |s: &[f32]| {
        q16(s)
            .iter()
            .map(|&x| x as f32 / 32768.0)
            .collect::<Vec<_>>()
    };
    let d = decode_bytes(wav_i16(&st, 2, 44100), Some("wav")).unwrap();
    check_exact(&d, &q(&st), 2, 44100, 1e-6);
    let d = decode_bytes(wav_i24(&st, 2, 48000), Some("wav")).unwrap();
    check_exact(&d, &st, 2, 48000, 2e-7);
    let d = decode_bytes(wav_f32(&st, 2, 96000), None).unwrap();
    check_exact(&d, &st, 2, 96000, 1e-7);
    let mono = sine(300.0, 0.4, 5000, 32000);
    let d = decode_bytes(wav_f32(&mono, 1, 32000), Some("wav")).unwrap();
    check_exact(&d, &mono, 1, 32000, 1e-7);
}

#[test]
fn flac_stereo_and_mono() {
    // 10000 frames = two full blocks and a short last one
    let st = stereo_test_signal(10000, 44100);
    let q: Vec<f32> = q16(&st).iter().map(|&x| x as f32 / 32768.0).collect();
    let d = decode_bytes(flac_verbatim(&st, 2, 44100), Some("flac")).unwrap();
    check_exact(&d, &q, 2, 44100, 1e-6);
    let mono = sine(300.0, 0.4, 5000, 48000);
    let qm: Vec<f32> = q16(&mono).iter().map(|&x| x as f32 / 32768.0).collect();
    let d = decode_bytes(flac_verbatim(&mono, 1, 48000), None).unwrap();
    check_exact(&d, &qm, 1, 48000, 1e-6);
}

#[test]
fn garbage_and_empty_are_errors() {
    assert!(decode_bytes(vec![0x42; 4096], Some("mp3")).is_err());
    assert!(decode_bytes(Vec::new(), None).is_err());
    assert!(decode_bytes(wav_i16(&[], 2, 44100), Some("wav")).is_err());
}

#[test]
fn resample_44k1_to_48k_keeps_length_and_level() {
    let src = sine(1000.0, 0.5, 44100 * 2, 44100);
    let d = decode_bytes(wav_f32(&src, 1, 44100), None).unwrap();
    let r = resample(&d, 48000).unwrap();
    assert_eq!(r.sample_rate, 48000);
    assert!(
        (r.frames() as i64 - 96000).abs() <= 2,
        "frames {}",
        r.frames()
    );
    let rms = |v: &[f32]| (v.iter().map(|x| x * x).sum::<f32>() / v.len() as f32).sqrt();
    let mid = &r.samples[4800..r.samples.len() - 4800];
    assert!(
        (rms(mid) - 0.5 / 2f32.sqrt()).abs() < 0.005,
        "rms {}",
        rms(mid)
    );
    // Frequency preserved: 1 kHz = 2000 zero crossings per second.
    let zc = mid.windows(2).filter(|w| w[0] <= 0.0 && w[1] > 0.0).count();
    let secs = mid.len() as f32 / 48000.0;
    assert!(
        (zc as f32 / secs - 1000.0).abs() < 5.0,
        "f = {}",
        zc as f32 / secs
    );
    // Same rate is a no-op.
    assert_eq!(resample(&r, 48000).unwrap(), r);
}
