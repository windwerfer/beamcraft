//! Upgrade check for a new FilmCraft rev (docs/agents/filmcraft.md): feeds a recorded Annex B
//! H.264 stream through `FilmDecoder` one access unit per call, like the viewers, and prints
//! the per-AU time, errors and output delay. Optionally writes the I420 output to compare it
//! byte for byte with FFmpeg's decoder.
//!
//! ```sh
//! BEAMCRAFT_FILM_BENCH=rec.h264 [BEAMCRAFT_FILM_YUV=out.yuv] \
//!   cargo test --release -p beamcraft-film --test bench -- --ignored --nocapture
//! ffmpeg -i rec.h264 -f rawvideo -pix_fmt yuv420p ref.yuv && cmp out.yuv ref.yuv
//! ```

use beamcraft_film::FilmDecoder;
use std::io::Write;
use std::time::Instant;

/// Splits an Annex B stream into access units: (start, end, is_idr). A new AU starts at a
/// non-VCL NAL after a VCL NAL, or at a slice with first_mb_in_slice = 0.
fn access_units(d: &[u8]) -> Vec<(usize, usize, bool)> {
    let mut codes = Vec::new(); // (start-code start, payload start)
    let mut i = 0;
    while i + 3 <= d.len() {
        if d[i..i + 3] == [0, 0, 1] {
            let sc = if i > 0 && d[i - 1] == 0 { i - 1 } else { i };
            codes.push((sc, i + 3));
            i += 3;
        } else {
            i += 1;
        }
    }
    let mut aus: Vec<(usize, usize, bool)> = Vec::new();
    let mut seen_vcl = false;
    for (k, &(sc, payload)) in codes.iter().enumerate() {
        let end = codes.get(k + 1).map_or(d.len(), |c| c.0);
        let Some(&hdr) = d.get(payload) else { continue };
        let t = hdr & 0x1f;
        let vcl = t == 1 || t == 5;
        let first_mb0 = vcl && d.get(payload + 1).is_some_and(|b| b & 0x80 != 0);
        if aus.is_empty() || (seen_vcl && (!vcl || first_mb0)) {
            aus.push((sc, end, false));
            seen_vcl = false;
        }
        let au = aus.last_mut().expect("pushed above");
        au.1 = end;
        if vcl {
            seen_vcl = true;
            au.2 |= t == 5;
        }
    }
    aus
}

fn report(name: &str, v: &mut [f64]) {
    if v.is_empty() {
        return;
    }
    v.sort_by(f64::total_cmp);
    let mean = v.iter().sum::<f64>() / v.len() as f64;
    let p = |q: f64| v[((v.len() - 1) as f64 * q) as usize];
    println!(
        "{name:>4}: n {:5}  mean {mean:6.2} ms  p50 {:6.2}  p95 {:6.2}  max {:6.2}",
        v.len(),
        p(0.5),
        p(0.95),
        p(1.0)
    );
}

#[test]
#[ignore = "needs a recording: BEAMCRAFT_FILM_BENCH=file.h264"]
fn bench_recording() {
    let Ok(path) = std::env::var("BEAMCRAFT_FILM_BENCH") else {
        println!("set BEAMCRAFT_FILM_BENCH to an Annex B .h264 file");
        return;
    };
    let data = std::fs::read(&path).expect("read the recording");
    let mut yuv = std::env::var("BEAMCRAFT_FILM_YUV")
        .ok()
        .map(|p| std::io::BufWriter::new(std::fs::File::create(p).expect("create the YUV output")));
    let aus = access_units(&data);
    let mut dec = FilmDecoder::default();
    let (mut t_idr, mut t_p) = (Vec::new(), Vec::new());
    let (mut pictures, mut empty, mut errors) = (0u64, 0u64, 0u64);
    for (i, &(s, e, idr)) in aus.iter().enumerate() {
        let t = Instant::now();
        let r = dec.decode(&data[s..e]);
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        if idr {
            t_idr.push(ms)
        } else {
            t_p.push(ms)
        }
        match r {
            Ok(Some(p)) => {
                pictures += 1;
                if let Some(w) = yuv.as_mut() {
                    for plane in [&p.y, &p.u, &p.v] {
                        w.write_all(plane).expect("write YUV");
                    }
                }
            }
            Ok(None) => empty += 1,
            Err(err) => {
                errors += 1;
                if errors <= 5 {
                    println!("AU {i}: {err}");
                }
            }
        }
    }
    println!(
        "{} access units ({} IDR): {pictures} pictures, {empty} calls without a picture \
         (output delay if > 0), {errors} errors",
        aus.len(),
        t_idr.len()
    );
    report("IDR", &mut t_idr);
    report("P", &mut t_p);
}
