use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};

/// Source frames queued before playback (re)starts: absorbs host scheduling jitter so a brief
/// stall does not turn into a stream of one-sample dropouts.
const PRIME_MS: usize = 66;
const MAX_MS: usize = 250;
const TRIM_MS: usize = 132;

/// Jitter buffer with an exact passthrough at the device rate and a fallback linear resampler.
/// Device-independent so its behaviour can be tested without an audio device.
struct Queue {
    rate: u32,
    frames: VecDeque<[i16; 2]>,
    /// Fractional position between `frames[0]` and `frames[1]`.
    pos: f64,
    primed: bool,
    /// Times playback ran dry and had to re-prime.
    underruns: u64,
    /// Queued source frames discarded because emulation outran the device.
    dropped: u64,
    error: Option<String>,
}

impl Queue {
    fn new(rate: u32) -> Self {
        Self {
            rate,
            frames: VecDeque::new(),
            pos: 0.0,
            primed: false,
            underruns: 0,
            dropped: 0,
            error: None,
        }
    }

    fn frames_for_ms(&self, ms: usize) -> usize {
        (self.rate as usize * ms / 1000).max(1)
    }

    fn push(&mut self, samples: impl IntoIterator<Item = [i16; 2]>) {
        self.frames.extend(samples);
        if self.frames.len() > self.frames_for_ms(MAX_MS) {
            let excess = self.frames.len() - self.frames_for_ms(TRIM_MS);
            self.frames.drain(..excess);
            self.dropped += excess as u64;
        }
    }

    fn stream_error(&mut self, error: cpal::StreamError) {
        // CPAL's ALSA backend prepares/recovers the stream after an xrun. A brief
        // scheduling delay is an audio glitch, not a reason to stop the game.
        if matches!(error, cpal::StreamError::BufferUnderrun) {
            self.underruns += 1;
        } else {
            self.error = Some(error.to_string());
        }
    }

    /// One device frame. `step` is source frames consumed per device frame (source rate / device rate).
    fn next(&mut self, step: f64) -> [i16; 2] {
        if !self.primed {
            if self.frames.len() < self.frames_for_ms(PRIME_MS) {
                return [0, 0];
            }
            self.primed = true;
        }
        if self.frames.len() < if step == 1.0 { 1 } else { 2 } {
            // Ran dry: play silence until the queue refills to the prime level (one event, not a
            // stream of one-sample dropouts).
            self.primed = false;
            self.underruns += 1;
            self.pos = 0.0;
            self.frames.clear();
            return [0, 0];
        }
        if step == 1.0 {
            self.pos = 0.0;
            return self.frames.pop_front().unwrap();
        }
        let (a, b) = (self.frames[0], self.frames[1]);
        let t = self.pos as f32;
        let mix = |c: usize| a[c] as f32 + (b[c] as f32 - a[c] as f32) * t;
        let out = [mix(0), mix(1)];
        self.pos += step;
        while self.pos >= 1.0 && self.frames.len() > 1 {
            self.pos -= 1.0;
            self.frames.pop_front();
        }
        [out[0] as i16, out[1] as i16]
    }
}

pub struct Output {
    shared: Arc<Mutex<Queue>>,
    _stream: cpal::Stream,
    rate: u32,
}

impl Output {
    pub fn new() -> Result<Self, String> {
        let device = cpal::default_host()
            .default_output_device()
            .ok_or("No audio output device; use --mute for diagnostics")?;
        let supported = device
            .default_output_config()
            .map_err(|e| format!("Audio configuration failed: {e}"))?;
        let format = supported.sample_format();
        let config: cpal::StreamConfig = supported.into();
        let shared = Arc::new(Mutex::new(Queue::new(config.sample_rate)));
        let stream = match format {
            cpal::SampleFormat::F32 => stream::<f32>(&device, &config, shared.clone()),
            cpal::SampleFormat::I16 => stream::<i16>(&device, &config, shared.clone()),
            cpal::SampleFormat::U16 => stream::<u16>(&device, &config, shared.clone()),
            _ => {
                return Err(format!(
                    "Unsupported audio sample format {format}; use --mute for diagnostics"
                ));
            }
        }
        .map_err(|e| format!("Cannot start audio output: {e}"))?;
        stream
            .play()
            .map_err(|e| format!("Cannot play audio: {e}"))?;
        Ok(Self {
            shared,
            _stream: stream,
            rate: config.sample_rate,
        })
    }

    /// Configure the machine to this rate before producing audio for this output.
    pub fn rate(&self) -> u32 {
        self.rate
    }

    pub fn push(&self, samples: Vec<[i16; 2]>) -> Result<(), String> {
        let mut q = self.shared.lock().map_err(|_| "Audio queue lock failed")?;
        if let Some(error) = &q.error {
            return Err(format!("Audio output failed: {error}"));
        }
        debug_assert_eq!(q.rate, self.rate());
        q.push(samples);
        Ok(())
    }

    /// (times playback ran dry, queued source frames discarded, source frames queued now).
    pub fn counters(&self) -> (u64, u64, usize) {
        self.shared
            .lock()
            .map(|q| (q.underruns, q.dropped, q.frames.len()))
            .unwrap_or((0, 0, 0))
    }
}

fn stream<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    shared: Arc<Mutex<Queue>>,
) -> Result<cpal::Stream, cpal::BuildStreamError>
where
    T: cpal::SizedSample + cpal::FromSample<i16>,
{
    let errors = shared.clone();
    let channels = config.channels as usize;
    let step = 1.0; // The host configures the SPU to Output::rate().
    device.build_output_stream(
        config,
        move |data: &mut [T], _| {
            let Ok(mut q) = shared.lock() else {
                data.fill(T::EQUILIBRIUM);
                return;
            };
            for output in data.chunks_mut(channels) {
                let sample = q.next(step);
                for (c, slot) in output.iter_mut().enumerate() {
                    let value = if channels == 1 {
                        ((sample[0] as i32 + sample[1] as i32) / 2) as i16
                    } else if c < 2 {
                        sample[c]
                    } else {
                        0
                    };
                    *slot = T::from_sample(value);
                }
            }
        },
        move |e| {
            if let Ok(mut q) = errors.lock() {
                q.stream_error(e);
            }
        },
        None,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_rate_passthrough_keeps_every_sample_and_buffer_durations() {
        for rate in [32768, 44100, 48000, 96000] {
            let mut q = Queue::new(rate);
            let count = q.frames_for_ms(PRIME_MS);
            assert_eq!(count, rate as usize * PRIME_MS / 1000);
            let input: Vec<_> = (0..count).map(|i| [i as i16, !(i as i16)]).collect();
            q.push(input.clone());
            for sample in input { assert_eq!(q.next(1.0), sample); }
            assert!(q.frames.is_empty());
            assert_eq!(q.underruns, 0, "last queued sample must be played");
            assert_eq!(q.next(1.0), [0, 0]);
            assert_eq!(q.underruns, 1);
            let max = q.frames_for_ms(MAX_MS);
            q.push(std::iter::repeat_n([42, -42], max + 1));
            assert_eq!(q.frames.len(), q.frames_for_ms(TRIM_MS));
            assert_eq!(q.dropped as usize, max + 1 - q.frames_for_ms(TRIM_MS));
            assert_eq!(q.next(1.0), [42, -42]);
        }
    }

    #[test]
    fn recoverable_device_underrun_does_not_stop_playback() {
        let mut q = Queue::new(recomp::spu::SAMPLE_RATE);
        q.stream_error(cpal::StreamError::BufferUnderrun);
        assert!(q.error.is_none());
        assert_eq!(q.underruns, 1);
        q.push(std::iter::repeat_n([123, -123], q.frames_for_ms(PRIME_MS)));
        assert_eq!(q.next(1.0), [123, -123]);
        q.stream_error(cpal::StreamError::DeviceNotAvailable);
        let fatal = q.error.clone();
        assert!(fatal.is_some());
        q.stream_error(cpal::StreamError::BufferUnderrun);
        assert_eq!(q.error, fatal, "an underrun must not clear a fatal device error");
    }

    fn sine(frames: usize, hz: f64) -> Vec<[i16; 2]> {
        let rate = recomp::spu::SAMPLE_RATE as f64;
        (0..frames)
            .map(|i| {
                let v = ((i as f64 / rate * hz * std::f64::consts::TAU).sin() * 12000.0) as i16;
                [v, v]
            })
            .collect()
    }

    /// Zero crossings per second of the left channel.
    fn frequency(out: &[[i16; 2]], device_rate: f64) -> f64 {
        let crossings = out
            .windows(2)
            .filter(|w| w[0][0] < 0 && w[1][0] >= 0)
            .count();
        crossings as f64 * device_rate / out.len() as f64
    }

    #[test]
    fn resampling_keeps_pitch_at_the_device_rate() {
        for device_rate in [44_100.0, 48_000.0] {
            let mut q = Queue::new(recomp::spu::SAMPLE_RATE);
            q.push(sine(recomp::spu::SAMPLE_RATE as usize / 5, 1000.0));
            let step = recomp::spu::SAMPLE_RATE as f64 / device_rate;
            let out: Vec<_> = (0..(device_rate as usize / 8)).map(|_| q.next(step)).collect();
            assert_eq!(q.underruns, 0);
            let hz = frequency(&out, device_rate);
            assert!((hz - 1000.0).abs() < 25.0, "{hz} Hz at device rate {device_rate}");
        }
    }

    #[test]
    fn waits_for_a_prime_before_playing_and_does_not_count_it_as_an_underrun() {
        let mut q = Queue::new(recomp::spu::SAMPLE_RATE);
        q.push(sine(q.frames_for_ms(PRIME_MS) - 1, 440.0));
        assert!((0..100).all(|_| q.next(0.7) == [0, 0]));
        assert_eq!(q.underruns, 0);
        q.push(sine(10, 440.0));
        assert!((0..400).any(|_| q.next(0.7) != [0, 0]));
    }

    #[test]
    fn running_dry_counts_once_then_reprimes() {
        let mut q = Queue::new(recomp::spu::SAMPLE_RATE);
        q.push(sine(q.frames_for_ms(PRIME_MS), 440.0));
        let mut dry = 0;
        for _ in 0..(q.frames_for_ms(PRIME_MS) * 3) {
            q.next(1.0);
            dry = q.underruns;
        }
        assert_eq!(dry, 1, "one dropout event, not one per device frame");
        q.push(sine(q.frames_for_ms(PRIME_MS), 440.0));
        assert!((0..50).any(|_| q.next(1.0) != [0, 0]), "playback resumes after refill");
    }

    #[test]
    fn overflow_discards_the_oldest_audio_in_one_trim() {
        let mut q = Queue::new(recomp::spu::SAMPLE_RATE);
        q.push(sine(q.frames_for_ms(MAX_MS) + 500, 440.0));
        assert_eq!(q.frames.len(), q.frames_for_ms(TRIM_MS));
        assert_eq!(q.dropped as usize, q.frames_for_ms(MAX_MS) + 500 - q.frames_for_ms(TRIM_MS));
    }
}
