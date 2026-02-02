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
/// Higher = sharper but more bandwidth. Terminal rendering is low-res anyway,
/// but more source pixels means better color accuracy when downsampled.
pub const MAX_CAPTURE_WIDTH: u32 = 1920;
/// Target frames per second
pub const TARGET_FPS: u32 = 10;
/// JPEG quality (1-100). Higher = sharper, more bandwidth.
pub const JPEG_QUALITY: u8 = 80;
