use anyhow::Result;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::sync::mpsc;

const OPUS_SAMPLE_RATE: u32 = 48000;
const OPUS_CHANNELS: u16 = 1;
const FRAME_SIZE: usize = 960; // 20ms at 48kHz mono

/// Lock-free ring buffer for audio playback
/// Avoids mutex contention between the network thread and ALSA callback
struct RingBuffer {
    buf: Vec<std::sync::atomic::AtomicU32>, // f32 bits stored as u32
    capacity: usize,
    read_pos: AtomicUsize,
    write_pos: AtomicUsize,
}

impl RingBuffer {
    fn new(capacity: usize) -> Self {
        let mut buf = Vec::with_capacity(capacity);
        for _ in 0..capacity {
            buf.push(std::sync::atomic::AtomicU32::new(0));
        }
        Self {
            buf,
            capacity,
            read_pos: AtomicUsize::new(0),
            write_pos: AtomicUsize::new(0),
        }
    }

    fn available(&self) -> usize {
        let w = self.write_pos.load(Ordering::Acquire);
        let r = self.read_pos.load(Ordering::Acquire);
        if w >= r { w - r } else { self.capacity - r + w }
    }

    fn free_space(&self) -> usize {
        self.capacity - 1 - self.available()
    }

    fn write(&self, samples: &[f32]) -> usize {
        let free = self.free_space();
        let to_write = samples.len().min(free);
        let mut pos = self.write_pos.load(Ordering::Relaxed);
        for i in 0..to_write {
            let bits = samples[i].to_bits();
            self.buf[pos].store(bits, Ordering::Relaxed);
            pos = (pos + 1) % self.capacity;
        }
        self.write_pos.store(pos, Ordering::Release);
        to_write
    }

    fn read(&self, output: &mut [f32]) -> usize {
        let avail = self.available();
        let to_read = output.len().min(avail);
        let mut pos = self.read_pos.load(Ordering::Relaxed);
        for i in 0..to_read {
            let bits = self.buf[pos].load(Ordering::Relaxed);
            output[i] = f32::from_bits(bits);
            pos = (pos + 1) % self.capacity;
        }
        self.read_pos.store(pos, Ordering::Release);
        to_read
    }

    /// Drop oldest samples to keep latency bounded
    fn trim_to(&self, max_samples: usize) {
        let avail = self.available();
        if avail > max_samples {
            let skip = avail - max_samples;
            let r = self.read_pos.load(Ordering::Relaxed);
            self.read_pos.store((r + skip) % self.capacity, Ordering::Release);
        }
    }
}

/// Manages audio capture and playback for voice calls
pub struct AudioPipeline {
    capture_rx: Option<mpsc::UnboundedReceiver<Vec<u8>>>,
    playback_tx: Option<mpsc::UnboundedSender<Vec<f32>>>,
    running: Arc<AtomicBool>,
    _capture_stream: Option<cpal::Stream>,
    _playback_stream: Option<cpal::Stream>,
}

impl AudioPipeline {
    pub fn start() -> Result<Self> {
        let host = cpal::default_host();
        let running = Arc::new(AtomicBool::new(true));

        let encoder = audiopus::coder::Encoder::new(
            audiopus::SampleRate::Hz48000,
            audiopus::Channels::Mono,
            audiopus::Application::Voip,
        ).map_err(|e| anyhow::anyhow!("Failed to create Opus encoder: {}", e))?;

        let decoder = audiopus::coder::Decoder::new(
            audiopus::SampleRate::Hz48000,
            audiopus::Channels::Mono,
        ).map_err(|e| anyhow::anyhow!("Failed to create Opus decoder: {}", e))?;

        // --- Capture ---
        let (capture_tx, capture_rx) = mpsc::unbounded_channel::<Vec<u8>>();
        let capture_stream = Self::start_capture(&host, encoder, capture_tx, running.clone())?;

        // --- Playback ---
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

        let default_config = device.default_input_config()
            .map_err(|e| anyhow::anyhow!("No default input config: {}", e))?;

        let device_sample_rate = default_config.sample_rate().0;
        let device_channels = default_config.channels();

        let config = cpal::StreamConfig {
            channels: device_channels,
            sample_rate: cpal::SampleRate(device_sample_rate),
            buffer_size: cpal::BufferSize::Default,
        };

        let device_frame_size = (device_sample_rate as usize * 20) / 1000 * device_channels as usize;

        let buffer = Arc::new(std::sync::Mutex::new(Vec::<f32>::with_capacity(device_frame_size * 2)));
        let buffer_clone = buffer.clone();

        // RNNoise denoiser — pure Rust, works on 48kHz, 480-sample (10ms) frames
        // Removes background noise (keyboard, fans, AC, etc.)
        let mut denoiser = nnnoiseless::DenoiseState::new();

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

                    let mono = if device_channels > 1 {
                        raw_frame.chunks(device_channels as usize)
                            .map(|ch| ch.iter().sum::<f32>() / device_channels as f32)
                            .collect::<Vec<f32>>()
                    } else {
                        raw_frame
                    };

                    let resampled = if device_sample_rate != OPUS_SAMPLE_RATE {
                        linear_resample(&mono, device_sample_rate, OPUS_SAMPLE_RATE, FRAME_SIZE)
                    } else {
                        let mut frame = mono;
                        frame.resize(FRAME_SIZE, 0.0);
                        frame
                    };

                    // Apply RNNoise denoising (480-sample chunks at 48kHz)
                    // Our 960-sample Opus frame = two RNNoise frames
                    let mut denoised = Vec::with_capacity(FRAME_SIZE);
                    for chunk in resampled.chunks(nnnoiseless::FRAME_SIZE) {
                        let mut rnn_buf = [0.0f32; nnnoiseless::FRAME_SIZE];
                        let len = chunk.len().min(nnnoiseless::FRAME_SIZE);
                        rnn_buf[..len].copy_from_slice(&chunk[..len]);
                        let mut output = [0.0f32; nnnoiseless::FRAME_SIZE];
                        denoiser.process_frame(&mut output, &rnn_buf);
                        denoised.extend_from_slice(&output[..len]);
                    }

                    let mut opus_out = vec![0u8; 4000];
                    match encoder.encode_float(&denoised, &mut opus_out) {
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

        // Lock-free ring buffer — 1 second capacity
        let ring_capacity = device_sample_rate as usize * device_channels as usize;
        let ring = Arc::new(RingBuffer::new(ring_capacity));
        let ring_writer = ring.clone();

        let running_clone = running.clone();
        let out_channels = device_channels;
        let out_rate = device_sample_rate;
        // Max latency: 500ms in samples
        let max_latency_samples = out_rate as usize * out_channels as usize / 2;

        // Writer thread: receives decoded PCM, resamples, writes to ring buffer
        std::thread::spawn(move || {
            // Use a simple blocking loop instead of a tokio runtime
            // to minimize latency and avoid runtime overhead
            let mut rx = rx;
            loop {
                match rx.blocking_recv() {
                    Some(samples) => {
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

                        // Trim if latency is growing too high
                        ring_writer.trim_to(max_latency_samples);
                        ring_writer.write(&expanded);
                    }
                    None => break,
                }
            }
        });

        // Playback callback — lock-free, just reads from ring buffer
        let mut last_sample = 0.0f32;
        let stream = device.build_output_stream(
            &config,
            move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                let read = ring.read(data);
                if read > 0 {
                    last_sample = data[read - 1];
                }
                // Fill remainder with fade-to-silence on underrun
                for sample in data[read..].iter_mut() {
                    last_sample *= 0.95;
                    *sample = last_sample;
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
