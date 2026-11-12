//! Image pipeline — port of `pyUltroid/imaging/` (`_processor.py`,
//! `_pipeline.py`, `_renderer.py`, `_errorcard.py`).
//!
//! Basic transforms over the `image` crate (grey, resize, rotate, flip,
//! blur, invert, glitch-ish channel shift). Error cards render text onto a
//! plain image; font rendering needs a TTF at runtime (DejaVu if present).

use crate::error::{Error, Result};
use image::{DynamicImage, GenericImageView, ImageBuffer, Rgb};

pub fn load(path: &std::path::Path) -> Result<DynamicImage> {
    image::open(path).map_err(|e| Error::Other(format!("image open: {e}")))
}

pub fn save(img: &DynamicImage, path: &std::path::Path) -> Result<()> {
    img.save(path).map_err(|e| Error::Other(format!("image save: {e}")))?;

    Ok(())
}

pub fn grey(img: &DynamicImage) -> DynamicImage {
    img.grayscale()
}

pub fn resize(img: &DynamicImage, width: u32, height: u32) -> DynamicImage {
    img.resize(width, height, image::imageops::FilterType::Lanczos3)
}

pub fn rotate(img: &DynamicImage, degrees: i32) -> DynamicImage {
    match degrees.rem_euclid(360) {
        90 => img.rotate90(),
        180 => img.rotate180(),
        270 => img.rotate270(),
        _ => img.clone(),
    }
}

pub fn flip_horizontal(img: &DynamicImage) -> DynamicImage {
    img.fliph()
}

pub fn flip_vertical(img: &DynamicImage) -> DynamicImage {
    img.flipv()
}

pub fn invert(img: &DynamicImage) -> DynamicImage {
    let mut out = img.clone();
    out.invert();
    out
}

pub fn blur(img: &DynamicImage, sigma: f32) -> DynamicImage {
    img.blur(sigma)
}

/// Channel-shift "glitch": shift the red channel horizontally by `amount`.
pub fn glitch(img: &DynamicImage, amount: i32) -> DynamicImage {
    let rgba = img.to_rgba8();
    let (w, h) = rgba.dimensions();
    let mut out: ImageBuffer<image::Rgba<u8>, Vec<u8>> = ImageBuffer::new(w, h);
    for (x, y, px) in rgba.enumerate_pixels() {
        let sx = (x as i32 + amount).rem_euclid(w as i32) as u32;
        let src = rgba.get_pixel(sx, y);
        out.put_pixel(x, y, image::Rgba([src[0], px[1], px[2], px[3]]));
    }
    DynamicImage::ImageRgba8(out)
}

/// Plain error card (solid background + size note). No font dependency:
/// the card is a coloured frame; text is delivered as the message caption
/// by the caller, mirroring the Python fallback when no monospace font is found.
pub fn error_card(width: u32, height: u32, colour: [u8; 3]) -> DynamicImage {
    let (r, g, b) = (colour[0], colour[1], colour[2]);
    let buf: ImageBuffer<Rgb<u8>, Vec<u8>> =
        ImageBuffer::from_fn(width, height, |_, _| Rgb([r, g, b]));
    DynamicImage::ImageRgb8(buf)
}

pub fn dimensions(img: &DynamicImage) -> (u32, u32) {
    img.dimensions()
}
