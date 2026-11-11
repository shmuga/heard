//! Timings for the heavy nodes on a long take: `cargo run --release --example bench -- file.wav`
use heard::ops::Op;
use std::sync::Arc;
use std::time::Instant;

fn main() {
    let path = std::env::args().nth(1).expect("audio file");
    let t = Instant::now();
    let (a, info) = heard::io::load(std::path::Path::new(&path)).expect("decode");
    println!(
        "decode {} ({}) {:?}",
        heard::audio::fmt_dur(info.seconds),
        info.format_label(),
        t.elapsed()
    );
    let a = Arc::new(a);
    for op in Op::catalog() {
        let t = Instant::now();
        let out = op.apply(vec![Some(a.clone()), Some(a.clone())]).unwrap();
        println!(
            "{:16} {:>8.0} ms  → {}",
            op.name(),
            t.elapsed().as_secs_f64() * 1000.0,
            heard::audio::fmt_dur(out.seconds())
        );
    }
    let t = Instant::now();
    let p = heard::ui::wave::WavePeaks::of(&a);
    println!("peaks {:?} ({} levels)", t.elapsed(), p.levels.len());
    let t = Instant::now();
    let _ = heard::ui::wave::spectrogram(&a, 4096);
    println!("spectrogram {:?}", t.elapsed());
}
