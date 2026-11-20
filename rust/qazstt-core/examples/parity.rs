//! Feature parity check: Rust `MelExtractor` vs NeMo's preprocessor output.
//!
//!   cargo run --release -p qazstt-core --no-default-features --example parity -- <16k mono wav> <check_features.npy>
//!
//! `check_features.npy` is written by `pipeline/07_export_onnx.py --check <wav>`
//! (float32, shape [80, T], C order). Exit code 0 when max |diff| < 0.1 and
//! mean |diff| < 0.005 (normalized-feature units; float32 STFT + log on near-silent
//! bins gives ~1e-2 noise — the real criterion is identical decoded text, stage 6).

use qazstt_core::{audio, features::{MelExtractor, N_MELS}};

fn read_npy_f32(path: &str) -> (Vec<usize>, Vec<f32>) {
    let b = std::fs::read(path).expect("read npy");
    assert_eq!(&b[..6], b"\x93NUMPY", "not an npy file");
    let (hlen, hstart) = if b[6] == 1 {
        (u16::from_le_bytes([b[8], b[9]]) as usize, 10)
    } else {
        (u32::from_le_bytes([b[8], b[9], b[10], b[11]]) as usize, 12)
    };
    let header = std::str::from_utf8(&b[hstart..hstart + hlen]).unwrap();
    assert!(header.contains("'<f4'"), "expected float32 little-endian, header: {header}");
    assert!(header.contains("'fortran_order': False"), "expected C order");
    let shape_s = header.split("'shape': (").nth(1).unwrap().split(')').next().unwrap();
    let shape: Vec<usize> = shape_s.split(',').filter_map(|s| s.trim().parse().ok()).collect();
    let data: Vec<f32> = b[hstart + hlen..]
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect();
    (shape, data)
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("usage: parity <wav> <check_features.npy>");
        std::process::exit(2);
    }
    let pcm = audio::read_wav(std::path::Path::new(&args[1])).expect("wav");
    let x = pcm.to_engine_format();
    let (ours, t) = MelExtractor::new().compute(&x);
    let (shape, theirs) = read_npy_f32(&args[2]);
    println!("rust  : [{N_MELS}, {t}]");
    println!("nemo  : {shape:?}");
    if shape != vec![N_MELS, t] {
        println!("FRAME COUNT MISMATCH");
    }
    let tn = shape[1].min(t);
    let (mut max_d, mut sum_d, mut n) = (0f32, 0f64, 0usize);
    let mut worst = (0, 0);
    for m in 0..N_MELS {
        for k in 0..tn {
            let d = (ours[m * t + k] - theirs[m * shape[1] + k]).abs();
            if d > max_d {
                max_d = d;
                worst = (m, k);
            }
            sum_d += d as f64;
            n += 1;
        }
    }
    println!("max |diff| = {max_d:.6} at mel {} frame {}", worst.0, worst.1);
    let mean_d = sum_d / n as f64;
    println!("mean|diff| = {mean_d:.6}");
    let ok = max_d < 0.1 && mean_d < 0.005 && shape == vec![N_MELS, t];
    println!("parity: {}", if ok { "OK" } else { "FAIL" });
    std::process::exit(if ok { 0 } else { 1 });
}
