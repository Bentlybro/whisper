pub mod capture;
pub mod viewer;

use serde::{Deserialize, Serialize};

/// Frame data sent over the wire (serialized inside the encrypted ciphertext)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScreenFrameData {
    pub width: u32,
    pub height: u32,
    /// JPEG-compressed frame data
    pub jpeg_data: Vec<u8>,
    /// Frame sequence number (for ordering/drop detection)
    pub seq: u64,
}

/// Max resolution for captured frames (width).
/// Frames are downscaled to fit this while preserving aspect ratio.
pub const MAX_CAPTURE_WIDTH: u32 = 1280;
/// Target frames per second
pub const TARGET_FPS: u32 = 8;
/// JPEG quality (1-100)
pub const JPEG_QUALITY: u8 = 60;
