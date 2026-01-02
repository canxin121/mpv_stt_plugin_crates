use log::{debug, trace};
use mpv_stt_common::{MpvSttError, Result};
use srtlib::{Subtitle, Subtitles};
use std::fmt;
use std::fs;
use std::path::Path;

// Re-export Timestamp for external use
pub use srtlib::Timestamp;

#[derive(Debug, Clone)]
pub struct SubtitleEntry {
    pub index: u32,
    pub start_time: Timestamp,
    pub end_time: Timestamp,
    pub text: String,
}

impl SubtitleEntry {
    fn from_srtlib(sub: Subtitle) -> Self {
        Self {
            index: sub.num as u32,
            start_time: sub.start_time,
            end_time: sub.end_time,
            text: sub.text,
        }
    }

    #[cfg(test)]
    fn to_srtlib(&self) -> Subtitle {
        Subtitle::new(
            self.index as usize,
            self.start_time,
            self.end_time,
            self.text.clone(),
        )
    }
}

impl fmt::Display for SubtitleEntry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}\n{} --> {}\n{}",
            self.index, self.start_time, self.end_time, self.text
        )
    }
}

pub struct SrtFile {
    pub entries: Vec<SubtitleEntry>,
}

impl SrtFile {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    pub fn parse<P: AsRef<Path>>(path: P) -> Result<Self> {
        trace!("Parsing SRT file: {}", path.as_ref().display());
        let subs = Subtitles::parse_from_file(path.as_ref(), None)
            .map_err(|e| MpvSttError::InvalidSrt(e.to_string()))?;

        let entries: Vec<SubtitleEntry> = subs
            .to_vec()
            .into_iter()
            .map(SubtitleEntry::from_srtlib)
            .collect();

        debug!("Parsed SRT file with {} entries", entries.len());
        Ok(Self { entries })
    }

    pub fn parse_content(content: &str) -> Result<Self> {
        let subs = Subtitles::parse_from_str(content.to_string())
            .map_err(|e| MpvSttError::InvalidSrt(e.to_string()))?;

        let entries: Vec<SubtitleEntry> = subs
            .to_vec()
            .into_iter()
            .map(SubtitleEntry::from_srtlib)
            .collect();

        Ok(Self { entries })
    }

    pub fn save<P: AsRef<Path>>(&self, path: P) -> Result<()> {
        trace!("Saving SRT file to: {}", path.as_ref().display());
        let content = self.to_string();
        fs::write(path, content)?;
        debug!("Saved SRT file with {} entries", self.entries.len());
        Ok(())
    }

    pub fn append_entry(&mut self, entry: SubtitleEntry) {
        self.entries.push(entry);
    }

    pub fn merge_bilingual(&mut self, translations: &[String]) {
        for (i, entry) in self.entries.iter_mut().enumerate() {
            if i < translations.len() && !translations[i].is_empty() {
                entry.text = format!("{}\n{}", entry.text, translations[i]);
            }
        }
    }
}

impl fmt::Display for SrtFile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, entry) in self.entries.iter().enumerate() {
            write!(f, "{}", entry)?;
            if i < self.entries.len() - 1 {
                write!(f, "\n\n")?;
            }
        }
        Ok(())
    }
}

pub fn offset_srt_file<P: AsRef<Path>>(
    input_path: P,
    output_path: P,
    offset_ms: i64,
) -> Result<()> {
    trace!("Offsetting SRT timestamps by {}ms", offset_ms);
    let mut subs = Subtitles::parse_from_file(input_path.as_ref(), None)
        .map_err(|e| MpvSttError::InvalidSrt(e.to_string()))?;

    for sub in &mut subs {
        sub.add_milliseconds(offset_ms);
    }

    subs.write_to_file(output_path.as_ref(), None)
        .map_err(|e| match e {
            srtlib::ParsingError::IOError(io_err) => MpvSttError::Io(io_err),
            _ => MpvSttError::InvalidSrt(e.to_string()),
        })?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_timestamp_parsing() {
        let ts = Timestamp::parse("01:23:45,678").unwrap();
        assert_eq!(ts.to_string(), "01:23:45,678");
    }

    #[test]
    fn test_subtitle_entry_conversion() {
        let entry = SubtitleEntry {
            index: 1,
            start_time: Timestamp::parse("00:00:10,500").unwrap(),
            end_time: Timestamp::parse("00:00:15,500").unwrap(),
            text: "Test subtitle".to_string(),
        };

        let srtlib_sub = entry.to_srtlib();
        assert_eq!(srtlib_sub.num, 1);
        assert_eq!(srtlib_sub.text, "Test subtitle");

        let converted_back = SubtitleEntry::from_srtlib(srtlib_sub);
        assert_eq!(converted_back.index, 1);
        assert_eq!(converted_back.start_time.to_string(), "00:00:10,500");
        assert_eq!(converted_back.end_time.to_string(), "00:00:15,500");
        assert_eq!(converted_back.text, "Test subtitle");
    }
}
