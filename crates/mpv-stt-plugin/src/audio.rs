use ffmpeg::format::Sample;
use ffmpeg::format::sample::Type as SampleType;
use ffmpeg::util::mathematics::rescale;
use ffmpeg::util::mathematics::rescale::Rescale;
use ffmpeg_next as ffmpeg;
use log::{debug, trace};
use mpv_stt_common::{MpvSttError, Result};
use std::path::Path;
use std::sync::{
    Arc, OnceLock,
    atomic::{AtomicU64, Ordering},
};
use std::time::{Duration, Instant};

static FFMPEG_INIT: OnceLock<std::result::Result<(), String>> = OnceLock::new();

fn ensure_ffmpeg() -> Result<()> {
    match FFMPEG_INIT.get_or_init(|| ffmpeg::init().map_err(|e| e.to_string())) {
        Ok(()) => Ok(()),
        Err(err) => Err(MpvSttError::AudioExtractionFailed(format!(
            "ffmpeg init failed: {err}"
        ))),
    }
}

fn ffmpeg_err(context: &str, err: impl std::fmt::Display) -> MpvSttError {
    MpvSttError::AudioExtractionFailed(format!("{context}: {err}"))
}

fn check_timeout(start: Instant, timeout: Duration, label: &str) -> Result<()> {
    if timeout.as_millis() == 0 {
        return Ok(());
    }
    if start.elapsed() > timeout {
        return Err(MpvSttError::ProcessTimeout(format!(
            "{label} timed out after {}ms",
            timeout.as_millis()
        )));
    }
    Ok(())
}

fn output_channel_layout(channels: u8) -> ffmpeg::channel_layout::ChannelLayout {
    match channels {
        1 => ffmpeg::channel_layout::ChannelLayout::MONO,
        2 => ffmpeg::channel_layout::ChannelLayout::STEREO,
        ch => ffmpeg::channel_layout::ChannelLayout::default(ch as i32),
    }
}

pub struct AudioExtractor {
    output_sample_rate: u32,
    output_channels: u8,
    ffmpeg_timeout: Duration,
    ffprobe_timeout: Duration,
    cancel_generation: Arc<AtomicU64>,
}

impl Default for AudioExtractor {
    fn default() -> Self {
        Self {
            output_sample_rate: 16000,
            output_channels: 1,
            ffmpeg_timeout: Duration::from_secs(30),
            ffprobe_timeout: Duration::from_secs(10),
            cancel_generation: Arc::new(AtomicU64::new(0)),
        }
    }
}

impl AudioExtractor {
    pub fn new(sample_rate: u32, channels: u8) -> Self {
        Self {
            output_sample_rate: sample_rate,
            output_channels: channels,
            ..Default::default()
        }
    }

    pub fn with_ffmpeg_timeout(mut self, timeout_ms: u64) -> Self {
        self.ffmpeg_timeout = Duration::from_millis(timeout_ms);
        self
    }

    pub fn with_ffprobe_timeout(mut self, timeout_ms: u64) -> Self {
        self.ffprobe_timeout = Duration::from_millis(timeout_ms);
        self
    }

    pub fn cancel_inflight(&self) {
        self.cancel_generation.fetch_add(1, Ordering::Relaxed);
    }

    fn check_cancel(&self, generation: u64) -> Result<()> {
        if self.cancel_generation.load(Ordering::Relaxed) != generation {
            return Err(MpvSttError::AudioExtractionCancelled);
        }
        Ok(())
    }

    /// Extract audio segment from media file using ffmpeg libraries
    pub fn extract_audio_segment<P: AsRef<Path>>(
        &self,
        input_path: P,
        output_path: P,
        start_ms: u64,
        duration_ms: u64,
    ) -> Result<()> {
        ensure_ffmpeg()?;
        let start_time = Instant::now();
        let run_generation = self.cancel_generation.load(Ordering::Relaxed);

        let input_str = input_path
            .as_ref()
            .to_str()
            .ok_or_else(|| MpvSttError::InvalidPath("Invalid input path".to_string()))?;
        let output_str = output_path
            .as_ref()
            .to_str()
            .ok_or_else(|| MpvSttError::InvalidPath("Invalid output path".to_string()))?;

        trace!(
            "Extracting audio: {}ms-{}ms from {} -> {}",
            start_ms,
            start_ms + duration_ms,
            input_str,
            output_str
        );

        let cancel_generation = Arc::clone(&self.cancel_generation);
        let ffmpeg_timeout = self.ffmpeg_timeout;
        let mut ictx = ffmpeg::format::input_with_interrupt(&input_path, move || {
            if ffmpeg_timeout.as_millis() != 0 && start_time.elapsed() > ffmpeg_timeout {
                return true;
            }
            cancel_generation.load(Ordering::Relaxed) != run_generation
        })
        .map_err(|e| ffmpeg_err("open input failed", e))?;

        check_timeout(start_time, self.ffmpeg_timeout, "ffmpeg")?;
        self.check_cancel(run_generation)?;

        let mut seeked = false;
        if start_ms > 0 {
            let position = (start_ms as i64).rescale((1, 1000), rescale::TIME_BASE);
            if let Err(err) = ictx.seek(position, ..position) {
                trace!("ffmpeg seek failed, falling back to decode+skip: {err}");
            } else {
                seeked = true;
            }
        }

        let input_stream = ictx
            .streams()
            .best(ffmpeg::media::Type::Audio)
            .ok_or_else(|| {
                MpvSttError::AudioExtractionFailed("No audio stream found".to_string())
            })?;
        let stream_index = input_stream.index();

        let context_decoder =
            ffmpeg::codec::context::Context::from_parameters(input_stream.parameters())
                .map_err(|e| ffmpeg_err("decoder context failed", e))?;
        let mut decoder = context_decoder
            .decoder()
            .audio()
            .map_err(|e| ffmpeg_err("audio decoder failed", e))?;

        let output_layout = output_channel_layout(self.output_channels);
        let output_format = Sample::I16(SampleType::Packed);
        let mut resampler = ffmpeg::software::resampling::Context::get(
            decoder.format(),
            decoder.channel_layout(),
            decoder.rate() as u32,
            output_format,
            output_layout,
            self.output_sample_rate,
        )
        .map_err(|e| ffmpeg_err("resampler init failed", e))?;

        let spec = hound::WavSpec {
            channels: self.output_channels as u16,
            sample_rate: self.output_sample_rate,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::create(output_path, spec)?;

        let start_frames = if seeked {
            0
        } else {
            start_ms
                .saturating_mul(self.output_sample_rate as u64)
                .saturating_div(1000)
        };
        let target_frames = duration_ms
            .saturating_mul(self.output_sample_rate as u64)
            .saturating_div(1000);

        let mut skipped_frames = 0u64;
        let mut written_frames = 0u64;

        let mut decoded = ffmpeg::frame::Audio::empty();

        for (stream, packet) in ictx.packets() {
            if stream.index() != stream_index {
                continue;
            }

            check_timeout(start_time, self.ffmpeg_timeout, "ffmpeg")?;
            self.check_cancel(run_generation)?;
            decoder
                .send_packet(&packet)
                .map_err(|e| ffmpeg_err("send packet failed", e))?;

            while decoder.receive_frame(&mut decoded).is_ok() {
                check_timeout(start_time, self.ffmpeg_timeout, "ffmpeg")?;
                self.check_cancel(run_generation)?;

                let mut resampled = ffmpeg::frame::Audio::empty();
                let _ = resampler
                    .run(&decoded, &mut resampled)
                    .map_err(|e| ffmpeg_err("resample failed", e))?;

                let frames = resampled.samples() as usize;
                if frames == 0 {
                    continue;
                }
                let channels = self.output_channels as usize;
                let data = resampled.data(0);
                let sample_count = data.len() / std::mem::size_of::<i16>();
                if sample_count < frames * channels {
                    return Err(MpvSttError::AudioExtractionFailed(
                        "resampled frame shorter than expected".to_string(),
                    ));
                }
                let samples = unsafe {
                    std::slice::from_raw_parts(data.as_ptr() as *const i16, sample_count)
                };

                for frame_idx in 0..frames {
                    if skipped_frames < start_frames {
                        skipped_frames += 1;
                        continue;
                    }
                    if target_frames > 0 && written_frames >= target_frames {
                        break;
                    }

                    let base = frame_idx * channels;
                    for ch in 0..channels {
                        writer.write_sample(samples[base + ch])?;
                    }
                    written_frames += 1;
                }

                if target_frames > 0 && written_frames >= target_frames {
                    break;
                }
            }

            if target_frames > 0 && written_frames >= target_frames {
                break;
            }
        }

        if target_frames == 0 || written_frames < target_frames {
            decoder
                .send_eof()
                .map_err(|e| ffmpeg_err("send eof failed", e))?;
            while decoder.receive_frame(&mut decoded).is_ok() {
                check_timeout(start_time, self.ffmpeg_timeout, "ffmpeg")?;
                self.check_cancel(run_generation)?;
                let mut resampled = ffmpeg::frame::Audio::empty();
                let _ = resampler
                    .run(&decoded, &mut resampled)
                    .map_err(|e| ffmpeg_err("resample failed", e))?;

                let frames = resampled.samples() as usize;
                if frames == 0 {
                    continue;
                }
                let channels = self.output_channels as usize;
                let data = resampled.data(0);
                let sample_count = data.len() / std::mem::size_of::<i16>();
                if sample_count < frames * channels {
                    return Err(MpvSttError::AudioExtractionFailed(
                        "resampled frame shorter than expected".to_string(),
                    ));
                }
                let samples = unsafe {
                    std::slice::from_raw_parts(data.as_ptr() as *const i16, sample_count)
                };

                for frame_idx in 0..frames {
                    if skipped_frames < start_frames {
                        skipped_frames += 1;
                        continue;
                    }
                    if target_frames > 0 && written_frames >= target_frames {
                        break;
                    }

                    let base = frame_idx * channels;
                    for ch in 0..channels {
                        writer.write_sample(samples[base + ch])?;
                    }
                    written_frames += 1;
                }

                if target_frames > 0 && written_frames >= target_frames {
                    break;
                }
            }
        }

        self.check_cancel(run_generation)?;
        writer.finalize()?;
        if target_frames > 0 && written_frames == 0 {
            return Err(MpvSttError::AudioExtractionFailed(
                "no audio samples decoded".to_string(),
            ));
        }

        debug!("Audio extraction completed successfully");
        Ok(())
    }

    /// Check if audio file exists and is valid
    pub fn validate_audio<P: AsRef<Path>>(&self, path: P) -> Result<bool> {
        ensure_ffmpeg()?;
        if !path.as_ref().exists() {
            return Ok(false);
        }

        let path_str = path
            .as_ref()
            .to_str()
            .ok_or_else(|| MpvSttError::InvalidPath("Invalid path".to_string()))?;

        let start_time = Instant::now();
        let ictx = ffmpeg::format::input(&path).map_err(|e| ffmpeg_err("open input failed", e))?;
        check_timeout(start_time, self.ffprobe_timeout, "ffprobe")?;

        let has_audio = ictx.streams().best(ffmpeg::media::Type::Audio).is_some();
        trace!("validate_audio({}): {}", path_str, has_audio);
        Ok(has_audio)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_audio_extractor_new() {
        let extractor = AudioExtractor::new(48000, 2);
        assert_eq!(extractor.output_sample_rate, 48000);
        assert_eq!(extractor.output_channels, 2);
    }

    #[test]
    fn test_audio_extractor_default() {
        let extractor = AudioExtractor::default();
        assert_eq!(extractor.output_sample_rate, 16000);
        assert_eq!(extractor.output_channels, 1);
    }
}
