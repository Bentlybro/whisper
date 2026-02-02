//! Screen share viewer — displays received frames in a separate OS window.
//!
//! Uses minifb to create a simple pixel buffer window. Runs in a dedicated
//! thread so the TUI stays responsive.

use anyhow::Result;
use image::codecs::jpeg::JpegDecoder;
use image::ImageDecoder;
use minifb::{Key, Window, WindowOptions};
use std::io::Cursor;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::mpsc;

use super::ScreenFrameData;

/// Screen share viewer — opens a window and displays frames
pub struct ScreenViewer {
    running: Arc<AtomicBool>,
    frame_tx: mpsc::UnboundedSender<ScreenFrameData>,
}

impl ScreenViewer {
    /// Start the viewer window in a background thread.
    /// Returns a handle with a channel to push frames into.
    pub fn start(peer_name: String) -> Result<Self> {
        let running = Arc::new(AtomicBool::new(true));
        let (frame_tx, frame_rx) = mpsc::unbounded_channel::<ScreenFrameData>();

        let running_clone = running.clone();
        std::thread::spawn(move || {
            viewer_loop(peer_name, frame_rx, running_clone);
        });

        Ok(Self { running, frame_tx })
    }

    /// Send a frame to the viewer
    pub fn send_frame(&self, frame: ScreenFrameData) -> bool {
        self.frame_tx.send(frame).is_ok()
    }

    /// Check if the viewer is still running
    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::Relaxed)
    }

    /// Stop the viewer
    pub fn stop(&self) {
        self.running.store(false, Ordering::Relaxed);
    }
}

impl Drop for ScreenViewer {
    fn drop(&mut self) {
        self.stop();
    }
}

fn viewer_loop(
    peer_name: String,
    mut frame_rx: mpsc::UnboundedReceiver<ScreenFrameData>,
    running: Arc<AtomicBool>,
) {
    let title = format!("🔒 WSP Screen Share — {}", peer_name);

    // Start with a placeholder size; resize on first frame
    let mut width: usize = 640;
    let mut height: usize = 480;
    let mut buffer: Vec<u32> = vec![0; width * height];

    let mut window = match Window::new(
        &title,
        width,
        height,
        WindowOptions {
            resize: true,
            scale_mode: minifb::ScaleMode::AspectRatioStretch,
            ..WindowOptions::default()
        },
    ) {
        Ok(w) => w,
        Err(e) => {
            eprintln!("Failed to create viewer window: {}", e);
            running.store(false, Ordering::Relaxed);
            return;
        }
    };

    // Limit update rate to ~30fps for the window
    window.set_target_fps(30);

    while running.load(Ordering::Relaxed) && window.is_open() && !window.is_key_down(Key::Escape) {
        // Drain all pending frames, keep only the latest
        let mut latest_frame: Option<ScreenFrameData> = None;
        while let Ok(frame) = frame_rx.try_recv() {
            latest_frame = Some(frame);
        }

        if let Some(frame) = latest_frame {
            let fw = frame.width as usize;
            let fh = frame.height as usize;

            // Decode JPEG → RGB
            if let Ok(pixels) = decode_jpeg_to_argb(&frame.jpeg_data, fw, fh) {
                // Resize if dimensions changed
                if fw != width || fh != height {
                    width = fw;
                    height = fh;
                }
                buffer = pixels;
            }
        }

        // Update window
        let _ = window.update_with_buffer(&buffer, width, height);
    }

    running.store(false, Ordering::Relaxed);
}

/// Decode JPEG data to a Vec<u32> in 0x00RRGGBB format (what minifb expects)
fn decode_jpeg_to_argb(jpeg_data: &[u8], _width: usize, _height: usize) -> Result<Vec<u32>> {
    let cursor = Cursor::new(jpeg_data);
    let decoder = JpegDecoder::new(cursor)
        .map_err(|e| anyhow::anyhow!("JPEG decode failed: {}", e))?;

    let (dw, dh) = decoder.dimensions();
    let total_bytes = decoder.total_bytes() as usize;

    let mut rgb_buf = vec![0u8; total_bytes];
    decoder
        .read_image(&mut rgb_buf)
        .map_err(|e| anyhow::anyhow!("JPEG read failed: {}", e))?;

    let actual_w = dw as usize;
    let actual_h = dh as usize;
    let pixel_count = actual_w * actual_h;

    let mut argb = Vec::with_capacity(pixel_count);
    for i in 0..pixel_count {
        let offset = i * 3;
        if offset + 2 < rgb_buf.len() {
            let r = rgb_buf[offset] as u32;
            let g = rgb_buf[offset + 1] as u32;
            let b = rgb_buf[offset + 2] as u32;
            argb.push((r << 16) | (g << 8) | b);
        } else {
            argb.push(0);
        }
    }

    Ok(argb)
}
