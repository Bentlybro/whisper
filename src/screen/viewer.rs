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
use ratatui_image::picker::{Picker, ProtocolType};
use ratatui_image::protocol::StatefulProtocol;
use std::io::Cursor;

use super::ScreenFrameData;

/// Create a Picker by querying terminal capabilities.
///
/// If `force_protocol` is Some, skip detection and use the specified protocol.
/// Otherwise, try auto-detection, then env-var heuristics, then fall back to halfblocks.
///
/// Should be called BEFORE entering raw mode / alternate screen.
pub fn create_picker(force_protocol: Option<&str>) -> Picker {
    // If user explicitly forced a protocol via --graphics flag
    if let Some(proto_name) = force_protocol {
        let proto_type = match proto_name.to_lowercase().as_str() {
            "sixel" => ProtocolType::Sixel,
            "kitty" => ProtocolType::Kitty,
            "iterm2" | "iterm" => ProtocolType::Iterm2,
            "halfblocks" | "half" | "text" => ProtocolType::Halfblocks,
            _ => {
                eprintln!(
                    "⚠️  Unknown graphics protocol '{}', using auto-detect",
                    proto_name
                );
                return auto_detect_picker();
            }
        };
        let mut picker = Picker::halfblocks();
        picker.set_protocol_type(proto_type);
        eprintln!("🖥️  Graphics: forced {:?}", proto_type);
        return picker;
    }

    auto_detect_picker()
}

fn auto_detect_picker() -> Picker {
    // Try the standard stdio query first
    match Picker::from_query_stdio() {
        Ok(picker) => {
            let proto = picker.protocol_type();
            eprintln!("🖥️  Graphics: detected {:?}", proto);
            picker
        }
        Err(_) => {
            // Query failed — try env var heuristics before falling back
            let picker = env_heuristic_picker();
            eprintln!("🖥️  Graphics: {:?} (env heuristic)", picker.protocol_type());
            picker
        }
    }
}

/// Try to guess the protocol from environment variables.
/// WezTerm, Kitty, iTerm2 all set identifiable env vars.
fn env_heuristic_picker() -> Picker {
    let term_program = std::env::var("TERM_PROGRAM").unwrap_or_default();
    let term = std::env::var("TERM").unwrap_or_default();

    let proto = if term_program.contains("WezTerm") {
        // WezTerm supports Sixel, Kitty, and iTerm2 — Sixel is most reliable
        ProtocolType::Sixel
    } else if term_program.contains("iTerm") {
        ProtocolType::Iterm2
    } else if term.contains("xterm-kitty") || term_program.contains("kitty") {
        ProtocolType::Kitty
    } else if term_program.contains("ghostty") || term_program.contains("Ghostty") {
        ProtocolType::Kitty
    } else {
        // Check TERM for sixel-capable terminals
        let wt = std::env::var("WT_SESSION").unwrap_or_default();
        if !wt.is_empty() {
            // Windows Terminal sets WT_SESSION
            ProtocolType::Sixel
        } else {
            ProtocolType::Halfblocks
        }
    };

    let mut picker = Picker::halfblocks();
    if proto != ProtocolType::Halfblocks {
        picker.set_protocol_type(proto);
    }
    picker
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
