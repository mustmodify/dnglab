//! How long a CR3 decode takes to stop after its cancellation token is set, at several points of the decode. Read only.
//!
//!   cargo run -p rawler --example crx_cancel_probe --release -- <file.CR3> [runs]

use std::time::{Duration, Instant};

use rawler::cancel::CancellationToken;
use rawler::rawsource::RawSource;

fn main() {
  let args: Vec<String> = std::env::args().collect();
  let path = std::path::Path::new(args.get(1).expect("usage: crx_cancel_probe <file.CR3> [runs]"));
  let runs: usize = args.get(2).and_then(|v| v.parse().ok()).unwrap_or(5);
  let source = RawSource::new(path).expect("open");
  let decoder = rawler::get_decoder(&source).expect("decoder");
  let params = rawler::decoders::RawDecodeParams::default();
  let never = CancellationToken::new();
  decoder.raw_image_cancellable(&source, &params, false, &never).expect("warm decode");
  let full = (0..3)
    .map(|_| {
      let t = Instant::now();
      decoder.raw_image_cancellable(&source, &params, false, &never).expect("decode");
      t.elapsed()
    })
    .min()
    .unwrap();
  println!("whole decode {:.0} ms", full.as_secs_f64() * 1e3);
  println!("{:>6} {:>9} {:>9} {:>9} {:>9}", "at", "set at ms", "median ms", "min ms", "max ms");
  for frac in [0.10, 0.25, 0.40, 0.50, 0.60, 0.75, 0.90] {
    let mut lat: Vec<f64> = Vec::new();
    let mut finished = 0;
    for _ in 0..runs {
      let token = CancellationToken::new();
      let (res, latency) = std::thread::scope(|s| {
        let h = s.spawn(|| rawler::get_decoder(&source).expect("decoder").raw_image_cancellable(&source, &params, false, &token));
        std::thread::sleep(Duration::from_secs_f64(full.as_secs_f64() * frac));
        let set = Instant::now();
        token.cancel();
        let r = h.join().unwrap();
        (r, set.elapsed())
      });
      if res.is_err() {
        lat.push(latency.as_secs_f64() * 1e3);
      } else {
        finished += 1;
      }
    }
    lat.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let f = |v: Option<&f64>| v.map_or("-".to_string(), |x| format!("{x:.1}"));
    println!("{:>5.0}% {:>9.0} {:>9} {:>9} {:>9}   ({} finished first)", frac * 100.0, full.as_secs_f64() * frac * 1e3, f(lat.get(lat.len() / 2)), f(lat.first()), f(lat.last()), finished);
  }
}
