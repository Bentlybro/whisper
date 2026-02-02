//! TUI-based screen share renderer using half-block characters.
//!
//! Each terminal cell represents 2 vertical pixels using the '▀' character:
//! - Foreground color = top pixel
//! - Background color = bottom pixel
//! This gives 2x vertical density while using 24-bit true color.

use anyhow::Result;
use image::codecs::jpeg::JpegDecoder;
use image::ImageDecoder;
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Style},
    widgets::Widget,
};
use std::io::Cursor;

use super::ScreenFrameData;

/// Decoded frame ready for TUI rendering
pub struct DecodedFrame {
    pub width: u32,
    pub height: u32,
    /// RGB pixel data (width * height * 3 bytes)
    pub rgb: Vec<u8>,
}

impl DecodedFrame {
    /// Decode a ScreenFrameData (JPEG) into raw RGB pixels
    pub fn from_frame(frame: &ScreenFrameData) -> Result<Self> {
        let cursor = Cursor::new(&frame.jpeg_data);
        let decoder =
            JpegDecoder::new(cursor).map_err(|e| anyhow::anyhow!("JPEG decode failed: {}", e))?;

        let (w, h) = decoder.dimensions();
        let total_bytes = decoder.total_bytes() as usize;

        let mut rgb = vec![0u8; total_bytes];
        decoder
            .read_image(&mut rgb)
            .map_err(|e| anyhow::anyhow!("JPEG read failed: {}", e))?;

        Ok(Self {
            width: w,
            height: h,
            rgb,
        })
    }

    /// Get a pixel (R, G, B) at (x, y), with bounds checking
    fn pixel(&self, x: u32, y: u32) -> (u8, u8, u8) {
        if x >= self.width || y >= self.height {
            return (0, 0, 0);
        }
        let offset = ((y * self.width + x) * 3) as usize;
        if offset + 2 < self.rgb.len() {
            (self.rgb[offset], self.rgb[offset + 1], self.rgb[offset + 2])
        } else {
            (0, 0, 0)
        }
    }
}

/// Ratatui widget that renders a decoded frame using half-block characters
pub struct ScreenWidget<'a> {
    frame: &'a DecodedFrame,
}

impl<'a> ScreenWidget<'a> {
    pub fn new(frame: &'a DecodedFrame) -> Self {
        Self { frame }
    }
}

impl<'a> Widget for ScreenWidget<'a> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.width == 0 || area.height == 0 {
            return;
        }

        let term_w = area.width as u32;
        let term_h = area.height as u32;

        // Each terminal cell = 2 vertical pixels
        let pixel_h = term_h * 2;
        let pixel_w = term_w;

        // Calculate scaling to fit the frame into the terminal area
        // while preserving aspect ratio
        let frame_aspect = self.frame.width as f64 / self.frame.height as f64;
        let term_aspect = pixel_w as f64 / pixel_h as f64;

        let (render_w, render_h) = if frame_aspect > term_aspect {
            // Frame is wider — fit to width
            let rw = pixel_w;
            let rh = (pixel_w as f64 / frame_aspect) as u32;
            (rw, rh)
        } else {
            // Frame is taller — fit to height
            let rh = pixel_h;
            let rw = (pixel_h as f64 * frame_aspect) as u32;
            (rw, rh)
        };

        // Center the image in the available area
        let x_offset = (pixel_w.saturating_sub(render_w)) / 2;
        let y_offset = (pixel_h.saturating_sub(render_h)) / 2;

        // The half-block character: top half is foreground, bottom half is background
        let half_block = "▀";

        for ty in 0..term_h {
            for tx in 0..term_w {
                let px = tx;
                let py_top = ty * 2;
                let py_bot = ty * 2 + 1;

                // Map terminal pixel position to source frame position
                let (top_r, top_g, top_b) = sample_pixel(
                    self.frame,
                    px,
                    py_top,
                    x_offset,
                    y_offset,
                    render_w,
                    render_h,
                );
                let (bot_r, bot_g, bot_b) = sample_pixel(
                    self.frame,
                    px,
                    py_bot,
                    x_offset,
                    y_offset,
                    render_w,
                    render_h,
                );

                let cell_x = area.x + tx as u16;
                let cell_y = area.y + ty as u16;

                if cell_x < area.x + area.width && cell_y < area.y + area.height {
                    let cell = &mut buf[(cell_x, cell_y)];
                    cell.set_symbol(half_block);
                    cell.set_style(
                        Style::default()
                            .fg(Color::Rgb(top_r, top_g, top_b))
                            .bg(Color::Rgb(bot_r, bot_g, bot_b)),
                    );
                }
            }
        }
    }
}

/// Sample a pixel from the frame, mapping terminal pixel coords to source coords.
/// Returns black for pixels outside the rendered area (letterbox/pillarbox).
fn sample_pixel(
    frame: &DecodedFrame,
    px: u32,
    py: u32,
    x_offset: u32,
    y_offset: u32,
    render_w: u32,
    render_h: u32,
) -> (u8, u8, u8) {
    // Check if this pixel is in the rendered area
    if px < x_offset || py < y_offset {
        return (0, 0, 0);
    }
    let rx = px - x_offset;
    let ry = py - y_offset;
    if rx >= render_w || ry >= render_h {
        return (0, 0, 0);
    }

    // Map to source frame coordinates (nearest-neighbor sampling)
    let src_x = (rx as u64 * frame.width as u64 / render_w as u64) as u32;
    let src_y = (ry as u64 * frame.height as u64 / render_h as u64) as u32;

    frame.pixel(src_x, src_y)
}
