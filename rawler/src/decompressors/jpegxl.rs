// SPDX-License-Identifier: LGPL-2.1
// Copyright 2025 Daniel Vogelbacher <daniel@chaospixel.com>

//! JPEG-XL decompressor for DNG strips and tiles.

use jxl_oxide::color::{ColourEncoding, ColourSpace, Primaries, TransferFunction, WhitePoint};
use jxl_oxide::{EnumColourEncoding, JxlImage, RenderingIntent};

use crate::decompressors::{Decompressor, LineIteratorMut};

/// Decompressor for JPEG-XL compressed DNG image data.
///
/// Only 8-bit and 16-bit JPEG-XL streams are currently supported. Other
/// bit depths are rejected because `jxl_oxide` scales samples to the full
/// storage-type range on decode, which would break black-level calculations
/// without a rescaling step that is not yet implemented.
pub struct JpegXLDecompressor {
  /// Bits per sample as declared in the TIFF IFD (not the JXL stream).
  bps: u32,
}

impl JpegXLDecompressor {
  /// Creates a new `JpegXLDecompressor` for the given TIFF bits-per-sample.
  pub fn new(bps: u32) -> Self {
    Self { bps }
  }
}

/// CIE xy chromaticities of the red, green and blue primaries and of the white point.
type Chroma = ([f64; 2], [f64; 2], [f64; 2], [f64; 2]);

const D65: [f64; 2] = [0.3127, 0.3290];
const SRGB_PRIMARIES: Chroma = ([0.640, 0.330], [0.300, 0.600], [0.150, 0.060], D65);
const REC2020_PRIMARIES: Chroma = ([0.708, 0.292], [0.170, 0.797], [0.131, 0.046], D65);
const P3_PRIMARIES: Chroma = ([0.680, 0.320], [0.265, 0.690], [0.150, 0.060], D65);

fn inverse3(m: [[f64; 3]; 3]) -> [[f64; 3]; 3] {
  let det = m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1]) - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0]) + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0]);
  let d = 1.0 / det;
  [
    [
      (m[1][1] * m[2][2] - m[1][2] * m[2][1]) * d,
      (m[0][2] * m[2][1] - m[0][1] * m[2][2]) * d,
      (m[0][1] * m[1][2] - m[0][2] * m[1][1]) * d,
    ],
    [
      (m[1][2] * m[2][0] - m[1][0] * m[2][2]) * d,
      (m[0][0] * m[2][2] - m[0][2] * m[2][0]) * d,
      (m[0][2] * m[1][0] - m[0][0] * m[1][2]) * d,
    ],
    [
      (m[1][0] * m[2][1] - m[1][1] * m[2][0]) * d,
      (m[0][1] * m[2][0] - m[0][0] * m[2][1]) * d,
      (m[0][0] * m[1][1] - m[0][1] * m[1][0]) * d,
    ],
  ]
}

fn mul3(a: [[f64; 3]; 3], b: [[f64; 3]; 3]) -> [[f64; 3]; 3] {
  let mut o = [[0.0; 3]; 3];
  for (i, row) in o.iter_mut().enumerate() {
    for (j, v) in row.iter_mut().enumerate() {
      *v = (0..3).map(|k| a[i][k] * b[k][j]).sum();
    }
  }
  o
}

/// Linear RGB of the given primaries to CIE XYZ (white point `w` maps to Y = 1).
fn rgb_to_xyz(c: Chroma) -> [[f64; 3]; 3] {
  let col = |p: [f64; 2]| [p[0] / p[1], 1.0, (1.0 - p[0] - p[1]) / p[1]];
  let (r, g, b, w) = (col(c.0), col(c.1), col(c.2), col(c.3));
  let p = [[r[0], g[0], b[0]], [r[1], g[1], b[1]], [r[2], g[2], b[2]]];
  let pi = inverse3(p);
  let s = [
    pi[0][0] * w[0] + pi[0][1] * w[1] + pi[0][2] * w[2],
    pi[1][0] * w[0] + pi[1][1] * w[1] + pi[1][2] * w[2],
    pi[2][0] * w[0] + pi[2][1] * w[1] + pi[2][2] * w[2],
  ];
  [[p[0][0] * s[0], p[0][1] * s[1], p[0][2] * s[2]], [p[1][0] * s[0], p[1][1] * s[1], p[1][2] * s[2]], [p[2][0] * s[0], p[2][1] * s[1], p[2][2] * s[2]]]
}

/// The matrix that takes linear sRGB (what the decoder is asked to deliver) to the linear RGB the stream declares
/// (`colour_encoding` of the codestream header): Lightroom's HDR DNGs declare Rec.2020 linear.
///
/// Only a linear RGB encoding with the D65 white point and sRGB, BT.2100 (Rec.2020) or P3 primaries is understood; any
/// other (an ICC profile, a custom white point or primaries, a transfer curve) is an error, not a guess.
fn srgb_to_declared(encoding: &ColourEncoding) -> Result<[[f32; 3]; 3], String> {
  let ColourEncoding::Enum(e) = encoding else {
    return Err("JPEG-XL: the stream declares its colour as an ICC profile, which is not supported".into());
  };
  if e.colour_space != ColourSpace::Rgb {
    return Err(format!("JPEG-XL: colour space {:?} is not supported (RGB only)", e.colour_space));
  }
  if e.white_point != WhitePoint::D65 {
    return Err(format!("JPEG-XL: white point {:?} is not supported (D65 only)", e.white_point));
  }
  if !matches!(e.tf, TransferFunction::Linear) {
    return Err(format!("JPEG-XL: transfer function {:?} is not supported (linear only)", e.tf));
  }
  let target = match e.primaries {
    Primaries::Srgb => SRGB_PRIMARIES,
    Primaries::Bt2100 => REC2020_PRIMARIES,
    Primaries::P3 => P3_PRIMARIES,
    Primaries::Custom { .. } => return Err("JPEG-XL: custom primaries are not supported".into()),
  };
  let m = mul3(inverse3(rgb_to_xyz(target)), rgb_to_xyz(SRGB_PRIMARIES));
  let mut o = [[0f32; 3]; 3];
  for i in 0..3 {
    for j in 0..3 {
      o[i][j] = m[i][j] as f32;
    }
  }
  Ok(o)
}

/// Decodes the floating point JPEG-XL tiles of a Lightroom HDR DNG (DNG 1.7: `SampleFormat` 3, `LinearRaw`, three samples per
/// pixel, each tile a bare lossy VarDCT codestream). The output is scene-linear `f32` in the colour space the stream declares
/// (Rec.2020 for Lightroom), unclamped: lossy coding leaves small negatives and values above 1.
///
/// Two settings matter and cost a day to find: `force_wide_buffers` (without it some tiles of some files fail to decode) and
/// asking the decoder for linear sRGB before rendering (without it the result is flat grey with no error). Linear sRGB is then
/// taken to the declared primaries by [`srgb_to_declared`], so nothing is clipped on the way.
impl<'a> Decompressor<'a, f32> for JpegXLDecompressor {
  fn decompress(&self, src: &[u8], skip_rows: usize, lines: impl LineIteratorMut<'a, f32>, line_width: usize) -> std::result::Result<(), String> {
    let mut image = JxlImage::builder()
      .force_wide_buffers(true)
      .read(src)
      .map_err(|err| format!("Failed to read JPEG-XL image: {:?}", err))?;
    let declared = image.image_header().metadata.colour_encoding.clone();
    let m = srgb_to_declared(&declared)?;
    image.request_color_encoding(EnumColourEncoding::srgb_linear(RenderingIntent::Relative));
    let frame = image.render_frame(0).map_err(|err| format!("Failed to render JPEG-XL image: {:?}", err))?;
    let fb = frame.image_all_channels();
    let (w, ch) = (fb.width(), fb.channels());
    if ch < 3 {
      return Err(format!("JPEG-XL tile has {ch} colour channels, 3 are needed"));
    }
    let buf = fb.buf();
    let tile_rows = fb.height();
    for (y, line) in lines.enumerate() {
      let row = y + skip_rows;
      if row >= tile_rows {
        break;
      }
      for (x, px) in line.chunks_exact_mut(3).take(w).enumerate() {
        let s = &buf[(row * w + x) * ch..];
        for (c, out) in px.iter_mut().enumerate() {
          *out = m[c][0] * s[0] + m[c][1] * s[1] + m[c][2] * s[2];
        }
      }
      debug_assert_eq!(line.len(), line_width);
    }
    Ok(())
  }

  fn can_skip_rows(&self) -> bool {
    false
  }
}

impl<'a> Decompressor<'a, u16> for JpegXLDecompressor {
  /// Decodes a JPEG-XL buffer into `u16` pixel lines.
  ///
  /// Renders frame 0 of the JXL image (alpha channel excluded) and writes
  /// samples row by row into `lines`, skipping the first `skip_rows` rows.
  ///
  /// * **8 bps** — samples are decoded to `u8` via a temporary buffer and
  ///   zero-extended to `u16` without scaling.
  /// * **9–16 bps** — samples are written directly into the `u16` output.
  ///
  /// # Errors
  /// Returns `Err` if the JXL image cannot be read or rendered.
  ///
  /// # Panics
  /// Panics if the JXL stream's internal bit depth is not 8 or 16, or if
  /// `bps` is outside the ranges 8 and 9–16.
  fn decompress(&self, src: &[u8], mut skip_rows: usize, lines: impl LineIteratorMut<'a, u16>, line_width: usize) -> std::result::Result<(), String> {
    let image = JxlImage::builder()
      .read(src)
      .map_err(|err| format!("Failed to read JPEG-XL image: {:?}", err))?;
    if let Some(header) = image.frame_header(0) {
      if header.bit_depth.bits_per_sample() != 8 && header.bit_depth.bits_per_sample() != 16 {
        // jxl_oxide scales the pixels into full range of storage type.
        // If we get e.g. 12 bit compressed data, output is scaled to 16 bit.
        // This breaks blacklevel scaling. We need to scale back to given bps (from TIFF).
        unimplemented!("JPEG-XL bit-depth {} not supported yet", header.bit_depth.bits_per_sample());
      }
      //eprintln!("JPEG-XL Bit-Depth: {:?}", header.bit_depth);
    }
    let frame = image.render_frame(0).map_err(|err| format!("Failed to render JPEG-XL image: {:?}", err))?;

    let mut stream = frame.stream_no_alpha();

    match self.bps {
      8 => {
        let mut tmp = vec![0_u8; line_width];

        for line in lines.skip(skip_rows) {
          while skip_rows > 0 {
            let written = stream.write_to_buffer(&mut tmp);
            assert_eq!(line.len(), written);
            skip_rows -= 1;
          }
          let written = stream.write_to_buffer(&mut tmp);
          assert_eq!(line.len(), written);
          for (p, x) in line.iter_mut().zip(tmp.iter()) {
            *p = *x as u16; // Only change storage format, you MUST NOT scale up!
          }
        }
      }
      9..=16 => {
        for line in lines.skip(skip_rows) {
          while skip_rows > 0 {
            let written = stream.write_to_buffer(line);
            assert_eq!(line.len(), written);
            skip_rows -= 1;
          }
          let written = stream.write_to_buffer(line);
          assert_eq!(line.len(), written);
        }
      }
      _ => unimplemented!(),
    }

    /*
    let all_ch = frame.image_all_channels();

    let pixbuf = all_ch.buf();
    for (line, buf) in lines.zip(pixbuf.chunks_exact(line_width).skip(skip_rows)) {
      for (p, f) in line.iter_mut().zip(buf.iter()) {
        //debug_assert!(*f <= (1.0 + f32::EPSILON));
        // *p = (f * u16::MAX as f32) as u16;
        // *p = *f as u16;
      }
    }
    */
    Ok(())
  }

  fn can_skip_rows(&self) -> bool {
    false
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  /// The matrix from sRGB to Rec.2020, as published (ITU-R BT.2087): derived here from the chromaticities.
  #[test]
  fn srgb_to_rec2020_is_the_published_matrix() {
    let enc = ColourEncoding::Enum(EnumColourEncoding {
      colour_space: ColourSpace::Rgb,
      white_point: WhitePoint::D65,
      primaries: Primaries::Bt2100,
      tf: TransferFunction::Linear,
      rendering_intent: RenderingIntent::Relative,
    });
    let m = srgb_to_declared(&enc).unwrap();
    let published = [[0.627404, 0.329283, 0.043313], [0.069097, 0.919540, 0.011362], [0.016391, 0.088013, 0.895595]];
    for i in 0..3 {
      for j in 0..3 {
        assert!((m[i][j] - published[i][j]).abs() < 2e-4, "[{i}][{j}] = {} vs {}", m[i][j], published[i][j]);
      }
      assert!((m[i].iter().sum::<f32>() - 1.0).abs() < 1e-4, "white stays white: row {i}");
    }
  }

  /// A real Lightroom HDR DNG (not in the repo: it is a photo): `RAWLER_HDR_DNG=<file>-HDR.dng cargo test -p rawler --release
  /// --lib real_hdr -- --ignored`. Every tile decodes, the frame is three float samples per pixel in 0..white level, and no
  /// channel is flat (the failure mode of asking the decoder for the wrong colour encoding).
  #[test]
  #[ignore]
  fn real_hdr_dng_decodes_to_scene_linear_float() {
    use crate::rawimage::RawImageData;
    let Ok(path) = std::env::var("RAWLER_HDR_DNG") else {
      eprintln!("skipped: set RAWLER_HDR_DNG to an HDR DNG");
      return;
    };
    let img = crate::decode_file(&path).expect("decodes");
    assert_eq!(img.cpp, 3);
    let RawImageData::Float(px) = &img.data else { panic!("float data") };
    assert_eq!(px.len(), img.width * img.height * 3);
    let white = img.whitelevel.0[0] as f32;
    let (mut lo, mut hi) = ([f32::MAX; 3], [f32::MIN; 3]);
    for p in px.chunks_exact(3) {
      for c in 0..3 {
        lo[c] = lo[c].min(p[c]);
        hi[c] = hi[c].max(p[c]);
      }
    }
    for c in 0..3 {
      assert!(hi[c] - lo[c] > white * 0.1, "channel {c} is flat: {} .. {}", lo[c], hi[c]);
      assert!(hi[c] < white * 1.1, "channel {c} is far above the white level: {}", hi[c]);
      assert!(lo[c] > -white * 0.1, "channel {c} is far below zero: {}", lo[c]);
    }
  }

  #[test]
  fn srgb_declared_is_the_identity_and_what_is_not_understood_is_refused() {
    let srgb_linear = EnumColourEncoding::srgb_linear(RenderingIntent::Relative);
    let m = srgb_to_declared(&ColourEncoding::Enum(srgb_linear.clone())).unwrap();
    for i in 0..3 {
      for j in 0..3 {
        assert!((m[i][j] - if i == j { 1.0 } else { 0.0 }).abs() < 1e-5);
      }
    }
    let curved = EnumColourEncoding { tf: TransferFunction::Srgb, ..srgb_linear.clone() };
    assert!(srgb_to_declared(&ColourEncoding::Enum(curved)).unwrap_err().contains("linear only"));
    let other_white = EnumColourEncoding { white_point: WhitePoint::E, ..srgb_linear };
    assert!(srgb_to_declared(&ColourEncoding::Enum(other_white)).unwrap_err().contains("D65"));
    assert!(srgb_to_declared(&ColourEncoding::IccProfile(ColourSpace::Rgb)).unwrap_err().contains("ICC"));
  }
}
