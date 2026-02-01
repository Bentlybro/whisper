use anyhow::Result;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::mpsc;

const SAMPLE_RATE: u32 = 48000;
const CHANNELS: u16 = 1;
const FRAME_SIZE: usize = 960; // 20ms at 48kHz mono

/// Manages audio capture and playback for voice calls
pub struct AudioPipeline {
    /// Sends encoded Opus frames out (to be encrypted and sent over wire)
    capture_rx: Option<mpsc::UnboundedReceiver<Vec<u8>>>,
    /// Receives decoded PCM audio for playback
    playback_tx: Option<mpsc::UnboundedSender<Vec<f32>>>,
    /// Flag to stop the pipeline
    running: Arc<AtomicBool>,
    /// cpal streams (kept alive — dropping stops them)
    _capture_stream: Option<cpal::Stream>,
    _playback_stream: Option<cpal::Stream>,
}

impl AudioPipeline {
    /// Start the audio pipeline. Returns the pipeline handle.
    /// - `capture_rx`: poll this for encoded Opus frames to send
    /// - `playback_tx`: push decoded PCM f32 samples here for playback
    pub fn start() -> Result<Self> {
        let host = cpal::default_host();
        let running = Arc::new(AtomicBool::new(true));

        // --- Opus encoder/decoder ---
        let encoder = audiopus::coder::Encoder::new(
            audiopus::SampleRate::Hz48000,
            audiopus::Channels::Mono,
            audiopus::Application::Voip,
        ).map_err(|e| anyhow::anyhow!("Failed to create Opus encoder: {}", e))?;

        let decoder = audiopus::coder::Decoder::new(
            audiopus::SampleRate::Hz48000,
            audiopus::Channels::Mono,
        ).map_err(|e| anyhow::anyhow!("Failed to create Opus decoder: {}", e))?;

        // --- Capture (microphone) ---
        let (capture_tx, capture_rx) = mpsc::unbounded_channel::<Vec<u8>>();
        let capture_stream = Self::start_capture(&host, encoder, capture_tx, running.clone())?;

        // --- Playback (speaker) ---
        let (playback_tx, playback_rx) = mpsc::unbounded_channel::<Vec<f32>>();
        let playback_stream = Self::start_playback(&host, decoder, playback_rx, running.clone())?;

        Ok(Self {
            capture_rx: Some(capture_rx),
            playback_tx: Some(playback_tx),
            running,
            _capture_stream: Some(capture_stream),
            _playback_stream: Some(playback_stream),
        })
    }

    /// Take the capture receiver (Opus-encoded frames ready to encrypt & send)
    pub fn take_capture_rx(&mut self) -> Option<mpsc::UnboundedReceiver<Vec<u8>>> {
        self.capture_rx.take()
    }

    /// Get a clone of the playback sender (send decoded PCM f32 samples here)
    pub fn playback_tx(&self) -> Option<mpsc::UnboundedSender<Vec<f32>>> {
        self.playback_tx.clone()
    }

    /// Stop the audio pipeline
    pub fn stop(&self) {
        self.running.store(false, Ordering::Relaxed);
    }

    /// Decode an Opus frame to PCM f32 samples
    pub fn decode_opus_frame(decoder: &mut audiopus::coder::Decoder, opus_data: &[u8]) -> Result<Vec<f32>> {
        use std::convert::TryFrom;
        let mut output = vec![0f32; FRAME_SIZE];
        let packet = audiopus::packet::Packet::try_from(opus_data)
            .map_err(|e| anyhow::anyhow!("Invalid Opus packet: {:?}", e))?;
        let mut_signals = audiopus::MutSignals::try_from(&mut output)
            .map_err(|e| anyhow::anyhow!("MutSignals error: {:?}", e))?;
        let decoded = decoder.decode_float(
            Some(packet),
            mut_signals,
            false, // no FEC
        ).map_err(|e| anyhow::anyhow!("Opus decode error: {}", e))?;
        output.truncate(decoded);
        Ok(output)
    }

    fn start_capture(
        host: &cpal::Host,
        encoder: audiopus::coder::Encoder,
        tx: mpsc::UnboundedSender<Vec<u8>>,
        running: Arc<AtomicBool>,
    ) -> Result<cpal::Stream> {
        let device = host.default_input_device()
            .ok_or_else(|| anyhow::anyhow!("No audio input device found"))?;

        let config = cpal::StreamConfig {
            channels: CHANNELS,
            sample_rate: cpal::SampleRate(SAMPLE_RATE),
            buffer_size: cpal::BufferSize::Fixed(FRAME_SIZE as u32),
        };

        // Buffer to accumulate exactly FRAME_SIZE samples
        let buffer = Arc::new(std::sync::Mutex::new(Vec::<f32>::with_capacity(FRAME_SIZE * 2)));
        let buffer_clone = buffer.clone();

        let stream = device.build_input_stream(
            &config,
            move |data: &[f32], _: &cpal::InputCallbackInfo| {
                if !running.load(Ordering::Relaxed) {
                    return;
                }
                let mut buf = buffer_clone.lock().unwrap();
                buf.extend_from_slice(data);

                // Process complete frames
                while buf.len() >= FRAME_SIZE {
                    let frame: Vec<f32> = buf.drain(..FRAME_SIZE).collect();
                    // Encode to Opus
                    let mut opus_out = vec![0u8; 4000]; // max opus frame
                    match encoder.encode_float(&frame, &mut opus_out) {
                        Ok(len) => {
                            opus_out.truncate(len);
                            let _ = tx.send(opus_out);
                        }
                        Err(_) => {} // skip frame on encode error
                    }
                }
            },
            |err| {
                eprintln!("Audio capture error: {}", err);
            },
            None, // no timeout
        )?;

        stream.play()?;
        Ok(stream)
    }

    fn start_playback(
        host: &cpal::Host,
        _decoder: audiopus::coder::Decoder,
        rx: mpsc::UnboundedReceiver<Vec<f32>>,
        running: Arc<AtomicBool>,
    ) -> Result<cpal::Stream> {
        let device = host.default_output_device()
            .ok_or_else(|| anyhow::anyhow!("No audio output device found"))?;

        let config = cpal::StreamConfig {
            channels: CHANNELS,
            sample_rate: cpal::SampleRate(SAMPLE_RATE),
            buffer_size: cpal::BufferSize::Default,
        };

        // Ring buffer for playback — accumulates decoded samples
        let playback_buf = Arc::new(std::sync::Mutex::new(std::collections::VecDeque::<f32>::new()));
        let playback_buf_writer = playback_buf.clone();

        // Spawn a thread to move samples from channel to ring buffer
        let running_clone = running.clone();
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            rt.block_on(async {
                let mut rx = rx;
                while let Some(samples) = rx.recv().await {
                    if !running_clone.load(Ordering::Relaxed) {
                        break;
                    }
                    let mut buf = playback_buf_writer.lock().unwrap();
                    // Limit buffer to ~200ms to avoid growing latency
                    let max_buf = SAMPLE_RATE as usize / 5; // 200ms
                    if buf.len() > max_buf {
                        let drain = buf.len() - max_buf;
                        buf.drain(..drain);
                    }
                    buf.extend(samples);
                }
            });
        });

        let stream = device.build_output_stream(
            &config,
            move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                let mut buf = playback_buf.lock().unwrap();
                for sample in data.iter_mut() {
                    *sample = buf.pop_front().unwrap_or(0.0);
                }
            },
            |err| {
                eprintln!("Audio playback error: {}", err);
            },
            None,
        )?;

        stream.play()?;
        Ok(stream)
    }
}

impl Drop for AudioPipeline {
    fn drop(&mut self) {
        self.stop();
    }
}
