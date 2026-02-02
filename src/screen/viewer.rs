//! Screen share viewer using ratatui-image.
//!
//! Automatically detects the best terminal graphics protocol:
//! - Sixel (Windows Terminal, xterm, foot, WezTerm, etc.)
//! - Kitty graphics protocol (Kitty, WezTerm, Ghostty)
//! - iTerm2 inline images (iTerm2, WezTerm)
//! - Halfblocks fallback (any terminal with 24-bit color)
//!
//! This gives actual pixel-perfect rendering on supported terminals.

use anyhow::Result;
use image::codecs::jpeg::JpegDecoder;
use image::{DynamicImage, ImageDecoder, RgbImage};
use ratatui_image::picker::Picker;
use ratatui_image::protocol::StatefulProtocol;
use std::io::Cursor;

use super::ScreenFrameData;

/// Create a Picker by querying terminal capabilities.
/// Must be called BEFORE entering raw mode / alternate screen.
pub fn create_picker() -> Picker {
    // Try to query the terminal for font size and protocol support
    match Picker::from_query_stdio() {
        Ok(picker) => picker,
        Err(_) => {
            // Fallback to halfblocks if query fails
            Picker::halfblocks()
        }
    }
}

/// Decoded frame ready for rendering
pub struct DecodedFrame {
    pub width: u32,
    pub height: u32,
    /// The image as a DynamicImage for ratatui-image
    pub image: DynamicImage,
}

impl DecodedFrame {
    /// Decode a ScreenFrameData (JPEG) into a DynamicImage
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

        let rgb_image: RgbImage =
            image::ImageBuffer::from_raw(w, h, rgb).ok_or_else(|| anyhow::anyhow!("Bad dims"))?;

        let image = DynamicImage::ImageRgb8(rgb_image);

        Ok(Self {
            width: w,
            height: h,
            image,
        })
    }

    /// Create a StatefulProtocol for rendering this frame with ratatui-image.
    /// The protocol handles encoding for the detected terminal graphics protocol.
    pub fn to_protocol(&self, picker: &mut Picker) -> StatefulProtocol {
        picker.new_resize_protocol(self.image.clone())
    }
}
