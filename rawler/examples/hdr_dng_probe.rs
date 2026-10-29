//! Decode a Lightroom HDR DNG (JPEG-XL half-float tiles) and report what came out. Read-only.
//!
//!   cargo run -p rawler --example hdr_dng_probe --release -- <file.dng> [out.jpg]
//!
//! Prints the size, samples per pixel, photometric, white level, per-channel min / mean / max, the share of negative
//! values and the decode time; with `out.jpg`, writes a 1/8 scale tone-mapped copy (gamma 2.2, white = mean x 3).

use std::time::Instant;

use rawler::rawimage::RawImageData;

fn main() {
  let args: Vec<String> = std::env::args().collect();
  let path = args.get(1).expect("usage: hdr_dng_probe <file.dng> [out.jpg]");
  let t = Instant::now();
  let img = match rawler::decode_file(path) {
    Ok(i) => i,
    Err(e) => {
      eprintln!("decode failed: {e}");
      std::process::exit(1);
    }
  };
  let secs = t.elapsed().as_secs_f64();
  println!("{}x{} cpp={} photometric={:?} whitelevel={:?} blacklevel={:?}", img.width, img.height, img.cpp, img.photometric, img.whitelevel, img.blacklevel);
  let RawImageData::Float(px) = &img.data else {
    println!("not float data");
    return;
  };
  let cpp = img.cpp;
  let n = px.len() / cpp;
  for c in 0..cpp {
    let (mut mn, mut mx, mut sum, mut neg) = (f32::MAX, f32::MIN, 0f64, 0usize);
    for p in px.chunks_exact(cpp) {
      let v = p[c];
      mn = mn.min(v);
      mx = mx.max(v);
      sum += v as f64;
      if v < 0.0 {
        neg += 1;
      }
    }
    println!("channel {c}: min {mn:.4} mean {:.4} max {mx:.2} negative {:.2}%", sum / n as f64, 100.0 * neg as f64 / n as f64);
  }
  println!("decoded in {secs:.2} s ({} pixels)", n);
  if let Some(out) = args.get(2) {
    let (w, h) = (img.width, img.height);
    let mean: f64 = px.iter().map(|&v| v as f64).sum::<f64>() / px.len() as f64;
    let white = (mean * 3.0) as f32;
    let (sw, sh) = (w / 8, h / 8);
    let mut rgb = image::RgbImage::new(sw as u32, sh as u32);
    for y in 0..sh {
      for x in 0..sw {
        let i = ((y * 8) * w + x * 8) * cpp;
        let enc = |v: f32| ((v / white).clamp(0.0, 1.0).powf(1.0 / 2.2) * 255.0) as u8;
        rgb.put_pixel(x as u32, y as u32, image::Rgb([enc(px[i]), enc(px[i + 1]), enc(px[i + 2])]));
      }
    }
    rgb.save(out).expect("save jpg");
  }
}
