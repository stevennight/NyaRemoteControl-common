//! Manual check: our own playback must show up in the normal loopback and be
//! absent from the process-excluding loopback.
//! `cargo run -p nya-win --example loopback_exclude`
use std::time::{Duration, Instant};

use nya_win::audio::{AudioRenderer, LoopbackCapture};

fn rms(v: &[f32]) -> f32 {
    if v.is_empty() {
        return 0.0;
    }
    (v.iter().map(|x| x * x).sum::<f32>() / v.len() as f32).sqrt()
}

fn main() -> anyhow::Result<()> {
    nya_win::com_init();
    let mut normal = LoopbackCapture::new()?;
    let mut excl = LoopbackCapture::excluding_process(std::env::args().nth(1).map_or(std::process::id(), |p| p.parse().unwrap()))?;
    let mut out = AudioRenderer::new()?;
    let (mut a, mut b) = (Vec::new(), Vec::new());
    let mut phase = 0f32;
    let start = Instant::now();
    while start.elapsed() < Duration::from_millis(1500) {
        let mut tone = Vec::new();
        for _ in 0..480 {
            let s = (phase * std::f32::consts::TAU).sin() * 0.05;
            phase = (phase + 440.0 / 48_000.0) % 1.0;
            tone.extend([s, s]);
        }
        let _ = out.write(&tone);
        std::thread::sleep(Duration::from_millis(10));
        normal.read(&mut a)?;
        excl.read(&mut b)?;
    }
    println!("normal loopback: {} samples, rms {:.4}", a.len(), rms(&a));
    println!("excluding self:  {} samples, rms {:.4}", b.len(), rms(&b));
    Ok(())
}
