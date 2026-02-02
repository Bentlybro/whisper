//! Screen capture pipeline.
//!
//! Captures the primary display, downscales to MAX_CAPTURE_WIDTH,
//! JPEG-compresses, and sends frames to a channel for encryption+transport.

use anyhow::Result;
use image::codecs::jpeg::JpegEncoder;
use image::{ImageBuffer, RgbImage};
use scrap::{Capturer, Display};
use std::io::Cursor;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

use super::{ScreenFrameData, JPEG_QUALITY, MAX_CAPTURE_WIDTH, TARGET_FPS};

/// Screen capture pipeline — runs in a dedicated thread.
/// Uses a bounded channel (capacity 2) to apply backpressure.
/// If the consumer can't keep up, old frames are dropped.
pub struct ScreenCapture {
    running: Arc<AtomicBool>,
    frame_rx: Option<mpsc::Receiver<ScreenFrameData>>,
}

impl ScreenCapture {
    /// Start capturing the primary display.
    /// Returns the capture handle with a channel of compressed frames.
    pub fn start() -> Result<Self> {
        // Verify a display exists before spawning the thread
        let display = Display::primary().map_err(|e| anyhow::anyhow!("No display found: {}", e))?;
        let _w = display.width();
        let _h = display.height();
        drop(display); // Drop here — Capturer is not Send on X11

        let running = Arc::new(AtomicBool::new(true));
        // Bounded channel (cap 2): if consumer is slow, old frames are dropped
        let (frame_tx, frame_rx) = mpsc::channel::<ScreenFrameData>(2);

        let running_clone = running.clone();
        std::thread::spawn(move || {
            // Create Capturer inside the thread (scrap::Capturer is !Send on X11)
            let display = match Display::primary() {
                Ok(d) => d,
                Err(e) => {
                    eprintln!("Screen capture: no display: {}", e);
                    return;
                }
            };
            let w = display.width();
            let h = display.height();
            let capturer = match Capturer::new(display) {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("Screen capture: failed to start: {}", e);
                    return;
                }
            };
            capture_loop(capturer, w, h, frame_tx, running_clone);
        });

        Ok(Self {
            running,
            frame_rx: Some(frame_rx),
        })
    }

    /// Take the frame receiver (can only be called once)
    pub fn take_frame_rx(&mut self) -> Option<mpsc::Receiver<ScreenFrameData>> {
        self.frame_rx.take()
    }

    /// Stop capturing
    pub fn stop(&self) {
        self.running.store(false, Ordering::Relaxed);
    }
}

impl Drop for ScreenCapture {
    fn drop(&mut self) {
        self.stop();
    }
}

fn capture_loop(
    mut capturer: Capturer,
    src_w: usize,
    src_h: usize,
    tx: mpsc::Sender<ScreenFrameData>,
    running: Arc<AtomicBool>,
) {
    let frame_interval = Duration::from_millis(1000 / TARGET_FPS as u64);
    let mut seq: u64 = 0;

    // Calculate output dimensions (downscale to MAX_CAPTURE_WIDTH, preserve aspect ratio)
    let (out_w, out_h) = if src_w as u32 > MAX_CAPTURE_WIDTH {
        let scale = MAX_CAPTURE_WIDTH as f64 / src_w as f64;
        let new_h = (src_h as f64 * scale) as u32;
        (MAX_CAPTURE_WIDTH, new_h)
    } else {
        (src_w as u32, src_h as u32)
    };

    while running.load(Ordering::Relaxed) {
        let frame_start = Instant::now();

        // Capture a frame
        match capturer.frame() {
            Ok(frame) => {
                // scrap gives us BGRA pixels (stride may include padding)
                let stride = frame.len() / src_h;

                // Convert BGRA → RGB and optionally downscale
                let rgb_image = bgra_to_rgb_scaled(&frame, src_w, src_h, stride, out_w, out_h);

                // JPEG compress
                match jpeg_encode(&rgb_image, out_w, out_h) {
                    Ok(jpeg_data) => {
                        let frame_data = ScreenFrameData {
                            width: out_w,
                            height: out_h,
                            jpeg_data,
                            seq,
                        };
                        seq += 1;

                        // try_send: if channel is full, drop this frame (backpressure)
                        match tx.try_send(frame_data) {
                            Ok(_) => {}
                            Err(mpsc::error::TrySendError::Full(_)) => {
                                // Consumer can't keep up — skip this frame
                            }
                            Err(mpsc::error::TrySendError::Closed(_)) => {
                                break; // Receiver dropped
                            }
                        }
                    }
                    Err(_) => {} // Skip frame on encode error
                }
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                // Frame not ready yet — just wait
                std::thread::sleep(Duration::from_millis(5));
                continue;
            }
            Err(_) => {
                // Capture error — retry after a short delay
                std::thread::sleep(Duration::from_millis(50));
                continue;
            }
        }

        // Maintain target frame rate
        let elapsed = frame_start.elapsed();
        if elapsed < frame_interval {
            std::thread::sleep(frame_interval - elapsed);
        }
    }
}

/// Convert BGRA buffer to RGB, optionally downscaling via nearest-neighbor
fn bgra_to_rgb_scaled(
    bgra: &[u8],
    src_w: usize,
    src_h: usize,
    stride: usize,
    dst_w: u32,
    dst_h: u32,
) -> Vec<u8> {
    let mut rgb = Vec::with_capacity((dst_w * dst_h * 3) as usize);

    for y in 0..dst_h {
        let src_y = (y as usize * src_h) / dst_h as usize;
        for x in 0..dst_w {
            let src_x = (x as usize * src_w) / dst_w as usize;
            let offset = src_y * stride + src_x * 4;
            if offset + 2 < bgra.len() {
                rgb.push(bgra[offset + 2]); // R (BGRA → R is at +2)
                rgb.push(bgra[offset + 1]); // G
                rgb.push(bgra[offset]);     // B
            } else {
                rgb.extend_from_slice(&[0, 0, 0]);
            }
        }
    }

    rgb
}

/// JPEG encode an RGB buffer
fn jpeg_encode(rgb: &[u8], width: u32, height: u32) -> Result<Vec<u8>> {
    let mut buf = Cursor::new(Vec::new());
    let encoder = JpegEncoder::new_with_quality(&mut buf, JPEG_QUALITY);

    let img: RgbImage = ImageBuffer::from_raw(width, height, rgb.to_vec())
        .ok_or_else(|| anyhow::anyhow!("Invalid image dimensions"))?;

    img.write_with_encoder(encoder)
        .map_err(|e| anyhow::anyhow!("JPEG encode failed: {}", e))?;

    Ok(buf.into_inner())
}
