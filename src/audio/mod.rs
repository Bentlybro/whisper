use anyhow::Result;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::mpsc;

const OPUS_SAMPLE_RATE: u32 = 48000;
const OPUS_CHANNELS: u16 = 1;
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

    pub fn take_capture_rx(&mut self) -> Option<mpsc::UnboundedReceiver<Vec<u8>>> {
        self.capture_rx.take()
    }

    pub fn playback_tx(&self) -> Option<mpsc::UnboundedSender<Vec<f32>>> {
        self.playback_tx.clone()
    }

    pub fn stop(&self) {
        self.running.store(false, Ordering::Relaxed);
    }

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
            false,
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

        // Use device's default config instead of forcing our own
        let default_config = device.default_input_config()
            .map_err(|e| anyhow::anyhow!("No default input config: {}", e))?;

        let device_sample_rate = default_config.sample_rate().0;
        let device_channels = default_config.channels();

        let config = cpal::StreamConfig {
            channels: device_channels,
            sample_rate: cpal::SampleRate(device_sample_rate),
            buffer_size: cpal::BufferSize::Default,
        };

        // Calculate how many device samples = one 20ms Opus frame
        let device_frame_size = (device_sample_rate as usize * 20) / 1000 * device_channels as usize;

        let buffer = Arc::new(std::sync::Mutex::new(Vec::<f32>::with_capacity(device_frame_size * 2)));
        let buffer_clone = buffer.clone();

        let stream = device.build_input_stream(
            &config,
            move |data: &[f32], _: &cpal::InputCallbackInfo| {
                if !running.load(Ordering::Relaxed) {
                    return;
                }
                let mut buf = buffer_clone.lock().unwrap();
                buf.extend_from_slice(data);

                while buf.len() >= device_frame_size {
                    let raw_frame: Vec<f32> = buf.drain(..device_frame_size).collect();

                    // Convert to mono if needed
                    let mono = if device_channels > 1 {
                        raw_frame.chunks(device_channels as usize)
                            .map(|ch| ch.iter().sum::<f32>() / device_channels as f32)
                            .collect::<Vec<f32>>()
                    } else {
                        raw_frame
                    };

                    // Resample to 48kHz if needed
                    let resampled = if device_sample_rate != OPUS_SAMPLE_RATE {
                        linear_resample(&mono, device_sample_rate, OPUS_SAMPLE_RATE, FRAME_SIZE)
                    } else {
                        // Might need to pad/truncate to exact FRAME_SIZE
                        let mut frame = mono;
                        frame.resize(FRAME_SIZE, 0.0);
                        frame
                    };

                    // Encode to Opus
                    let mut opus_out = vec![0u8; 4000];
                    match encoder.encode_float(&resampled, &mut opus_out) {
                        Ok(len) => {
                            opus_out.truncate(len);
                            let _ = tx.send(opus_out);
                        }
                        Err(_) => {}
                    }
                }
            },
            |err| {
                eprintln!("Audio capture error: {}", err);
            },
            None,
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

        let default_config = device.default_output_config()
            .map_err(|e| anyhow::anyhow!("No default output config: {}", e))?;

        let device_sample_rate = default_config.sample_rate().0;
        let device_channels = default_config.channels();

        let config = cpal::StreamConfig {
            channels: device_channels,
            sample_rate: cpal::SampleRate(device_sample_rate),
            buffer_size: cpal::BufferSize::Default,
        };

        // Ring buffer for playback
        let playback_buf = Arc::new(std::sync::Mutex::new(std::collections::VecDeque::<f32>::new()));
        let playback_buf_writer = playback_buf.clone();

        let running_clone = running.clone();
        let out_channels = device_channels;
        let out_rate = device_sample_rate;

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

                    // Resample from 48kHz to device rate if needed
                    let resampled = if out_rate != OPUS_SAMPLE_RATE {
                        let target_len = (samples.len() as u64 * out_rate as u64 / OPUS_SAMPLE_RATE as u64) as usize;
                        linear_resample(&samples, OPUS_SAMPLE_RATE, out_rate, target_len)
                    } else {
                        samples
                    };

                    // Expand mono to multi-channel if needed
                    let expanded = if out_channels > 1 {
                        resampled.iter()
                            .flat_map(|&s| std::iter::repeat(s).take(out_channels as usize))
                            .collect::<Vec<f32>>()
                    } else {
                        resampled
                    };

                    let mut buf = playback_buf_writer.lock().unwrap();
                    // Soft limit at 500ms — only trim if we exceed it,
                    // and trim gently back to 300ms to avoid audible gaps
                    let max_buf = out_rate as usize * out_channels as usize / 2; // 500ms
                    let target_buf = out_rate as usize * out_channels as usize * 3 / 10; // 300ms
                    if buf.len() > max_buf {
                        let drain = buf.len() - target_buf;
                        buf.drain(..drain);
                    }
                    buf.extend(expanded);
                }
            });
        });

        let stream = device.build_output_stream(
            &config,
            move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                let mut buf = playback_buf.lock().unwrap();
                let mut last_sample = 0.0f32;
                for sample in data.iter_mut() {
                    if let Some(s) = buf.pop_front() {
                        last_sample = s;
                        *sample = s;
                    } else {
                        // Underrun — fade to silence instead of hard cut
                        last_sample *= 0.95;
                        *sample = last_sample;
                    }
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

/// Simple linear interpolation resampler
fn linear_resample(input: &[f32], from_rate: u32, to_rate: u32, output_len: usize) -> Vec<f32> {
    if input.is_empty() {
        return vec![0.0; output_len];
    }
    let ratio = from_rate as f64 / to_rate as f64;
    (0..output_len).map(|i| {
        let src_pos = i as f64 * ratio;
        let idx = src_pos as usize;
        let frac = src_pos - idx as f64;
        let a = input.get(idx).copied().unwrap_or(0.0);
        let b = input.get(idx + 1).copied().unwrap_or(a);
        a + (b - a) * frac as f32
    }).collect()
}
