//! Raster images (RGBA) and coverage masks (A8), plus PNG encode/decode.

use crate::color::Color;
use std::io::{self, Write};

/// An 8-bit coverage mask (glyphs, icons).
#[derive(Clone, Debug, Default)]
pub struct Mask {
    /// Width in pixels.
    pub w: u32,
    /// Height in pixels.
    pub h: u32,
    /// Row-major coverage, `w * h` bytes.
    pub data: Vec<u8>,
}

impl Mask {
    /// A zeroed mask.
    pub fn new(w: u32, h: u32) -> Self {
        Mask {
            w,
            h,
            data: vec![0; (w * h) as usize],
        }
    }
}

/// A straight-alpha RGBA image in the canvas pixel packing.
#[derive(Clone, Debug, Default)]
pub struct Image {
    /// Width in pixels.
    pub w: u32,
    /// Height in pixels.
    pub h: u32,
    px: Vec<u32>,
}

/// Errors from PNG decoding.
#[derive(Debug)]
pub enum ImageError {
    /// The PNG stream could not be decoded.
    Decode(png::DecodingError),
    /// The PNG uses a colour type this decoder does not handle.
    Unsupported,
}

impl std::fmt::Display for ImageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ImageError::Decode(e) => write!(f, "png decode: {e}"),
            ImageError::Unsupported => write!(f, "unsupported png colour type"),
        }
    }
}

impl std::error::Error for ImageError {}

impl Image {
    /// Wraps packed pixels; `px.len()` must equal `w * h`.
    pub fn from_packed(w: u32, h: u32, px: Vec<u32>) -> Self {
        assert_eq!(px.len(), (w * h) as usize, "image buffer size mismatch");
        Image { w, h, px }
    }

    /// Reads one pixel.
    #[inline]
    pub fn get(&self, x: u32, y: u32) -> Color {
        Color::unpack(self.px[(y * self.w + x) as usize])
    }

    /// Packed pixels.
    pub fn pixels(&self) -> &[u32] {
        &self.px
    }

    /// Decodes a PNG (8-bit grey, grey+alpha, RGB, RGBA or palette).
    pub fn decode_png(bytes: &[u8]) -> Result<Image, ImageError> {
        let mut dec = png::Decoder::new(io::Cursor::new(bytes));
        dec.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
        let mut reader = dec.read_info().map_err(ImageError::Decode)?;
        let size = reader.output_buffer_size().ok_or(ImageError::Unsupported)?;
        let mut buf = vec![0; size];
        let info = reader.next_frame(&mut buf).map_err(ImageError::Decode)?;
        let (w, h) = (info.width, info.height);
        let n = (w * h) as usize;
        let mut px = Vec::with_capacity(n);
        let data = &buf[..info.buffer_size()];
        match info.color_type {
            png::ColorType::Rgba => px.extend(
                data.chunks_exact(4)
                    .map(|c| Color::rgba(c[0], c[1], c[2], c[3]).pack()),
            ),
            png::ColorType::Rgb => px.extend(
                data.chunks_exact(3)
                    .map(|c| Color::rgb(c[0], c[1], c[2]).pack()),
            ),
            png::ColorType::GrayscaleAlpha => px.extend(
                data.chunks_exact(2)
                    .map(|c| Color::rgba(c[0], c[0], c[0], c[1]).pack()),
            ),
            png::ColorType::Grayscale => {
                px.extend(data.iter().map(|&g| Color::rgb(g, g, g).pack()))
            }
            png::ColorType::Indexed => return Err(ImageError::Unsupported),
        }
        if px.len() != n {
            return Err(ImageError::Unsupported);
        }
        Ok(Image { w, h, px })
    }

    /// Encodes as an 8-bit RGB PNG (alpha is dropped; canvases are opaque).
    pub fn write_png_rgb<W: Write>(&self, out: W) -> io::Result<()> {
        let mut enc = png::Encoder::new(out, self.w, self.h);
        enc.set_color(png::ColorType::Rgb);
        enc.set_depth(png::BitDepth::Eight);
        enc.set_compression(png::Compression::High);
        let mut w = enc.write_header().map_err(io::Error::other)?;
        let mut rgb = Vec::with_capacity(self.px.len() * 3);
        for &p in &self.px {
            let c = Color::unpack(p);
            rgb.extend_from_slice(&[c.r, c.g, c.b]);
        }
        w.write_image_data(&rgb).map_err(io::Error::other)?;
        w.finish().map_err(io::Error::other)
    }

    /// Writes an RGB PNG file.
    pub fn save_png(&self, path: &std::path::Path) -> io::Result<()> {
        let f = std::fs::File::create(path)?;
        self.write_png_rgb(io::BufWriter::new(f))
    }

    /// Loads a PNG file.
    pub fn load_png(path: &std::path::Path) -> Result<Image, Box<dyn std::error::Error>> {
        Ok(Image::decode_png(&std::fs::read(path)?)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn png_roundtrip() {
        let img = Image::from_packed(
            2,
            1,
            vec![
                Color::rgb(10, 20, 30).pack(),
                Color::rgb(200, 100, 0).pack(),
            ],
        );
        let mut buf = Vec::new();
        img.write_png_rgb(&mut buf).unwrap();
        let back = Image::decode_png(&buf).unwrap();
        assert_eq!(back.get(0, 0), Color::rgb(10, 20, 30));
        assert_eq!(back.get(1, 0), Color::rgb(200, 100, 0));
    }
}
