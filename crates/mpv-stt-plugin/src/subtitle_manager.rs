use log::{debug, trace};
use mpv_stt_common::Result;
use mpv_stt_srt::{SrtFile, SubtitleEntry, Timestamp};
use std::collections::BTreeMap;
use std::path::Path;

/// Manages subtitles in memory and syncs to disk
pub struct SubtitleManager {
    /// Subtitles indexed by start time in milliseconds
    entries: BTreeMap<u32, SubtitleEntry>,
    next_index: u32,
}

impl SubtitleManager {
    pub fn new() -> Self {
        Self {
            entries: BTreeMap::new(),
            next_index: 1,
        }
    }

    /// Add a new subtitle entry
    pub fn add_entry(&mut self, start_ms: u32, entry: SubtitleEntry) {
        self.entries.insert(start_ms, entry);
    }

    /// Add multiple entries from an SRT file
    pub fn add_from_srt(&mut self, srt: &SrtFile) {
        trace!("Adding {} entries from SRT file", srt.entries.len());
        for entry in &srt.entries {
            let start_ms = Self::timestamp_to_millis(entry.start_time);
            self.entries.insert(start_ms, entry.clone());
        }
        debug!("Total subtitles in manager: {}", self.entries.len());
    }

    /// Remove all entries after a given timestamp (for seek backward)
    pub fn remove_after(&mut self, start_ms: u32) {
        let before_count = self.entries.len();
        // Keep entries at or before the seek target. If we drop entries that start exactly at
        // `start_ms`, seeking to the start of a chunk can make the first subtitle "disappear".
        self.entries.retain(|k, _| *k <= start_ms);
        let removed = before_count - self.entries.len();
        if removed > 0 {
            debug!("Removed {} entries after {}ms", removed, start_ms);
        }
    }

    /// Remove all entries before a given timestamp (for seek forward)
    pub fn remove_before(&mut self, start_ms: u32) {
        self.entries.retain(|k, _| *k >= start_ms);
    }

    /// Update an entry with translation (for async translation)
    pub fn update_translation(&mut self, start_ms: u32, translation: &str) {
        if translation.trim().is_empty() {
            trace!("Skipping empty translation for entry at {}ms", start_ms);
            return;
        }
        if let Some(entry) = self.entries.get_mut(&start_ms) {
            // Check if translation already exists (avoid duplicates)
            let normalized = translation.trim();
            let already_present = entry.text.lines().any(|line| line.trim() == normalized);
            if !already_present {
                entry.text = format!("{}\n{}", entry.text, translation);
                trace!("Updated translation for entry at {}ms", start_ms);
            }
        } else {
            debug!("Entry not found for translation update at {}ms", start_ms);
        }
    }

    /// Clear all entries
    pub fn clear(&mut self) {
        self.entries.clear();
        self.next_index = 1;
    }

    /// Write all subtitles to file
    pub fn save_to_file<P: AsRef<Path>>(&mut self, path: P) -> Result<()> {
        trace!("Saving {} subtitle entries to file", self.entries.len());
        let mut srt = SrtFile::new();

        // Reindex entries sequentially
        self.next_index = 1;
        for (_, entry) in self.entries.iter_mut() {
            entry.index = self.next_index;
            srt.append_entry(entry.clone());
            self.next_index += 1;
        }

        srt.save(path)?;
        Ok(())
    }

    /// Get number of entries
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Get entries within [start_ms, end_ms)
    pub fn entries_in_range(&self, start_ms: u32, end_ms: u32) -> Vec<(u32, SubtitleEntry)> {
        self.entries
            .range(start_ms..end_ms)
            .map(|(k, v)| (*k, v.clone()))
            .collect()
    }

    /// Check if empty
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    fn timestamp_to_millis(ts: Timestamp) -> u32 {
        let (h, m, s, ms) = ts.get();
        Timestamp::convert_to_milliseconds(h, m, s, ms)
    }

    /// Check if a subtitle text already contains a translation line.
    pub fn text_has_translation(text: &str) -> bool {
        let mut lines = text.lines().filter(|line| !line.trim().is_empty());
        lines.next().is_some() && lines.next().is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_timestamp_to_millis() {
        assert_eq!(
            SubtitleManager::timestamp_to_millis(Timestamp::parse("00:00:10,500").unwrap()),
            10_500
        );
        assert_eq!(
            SubtitleManager::timestamp_to_millis(Timestamp::parse("00:01:30.250").unwrap()),
            90_250
        );
        assert_eq!(
            SubtitleManager::timestamp_to_millis(Timestamp::parse("01:02:03,456").unwrap()),
            3_723_456
        );
    }

    #[test]
    fn test_subtitle_manager() {
        let mut manager = SubtitleManager::new();

        let entry1 = SubtitleEntry {
            index: 1,
            start_time: Timestamp::parse("00:00:00,000").unwrap(),
            end_time: Timestamp::parse("00:00:05,000").unwrap(),
            text: "First subtitle".to_string(),
        };

        manager.add_entry(0, entry1);
        assert_eq!(manager.len(), 1);

        manager.clear();
        assert!(manager.is_empty());
    }

    #[test]
    fn test_remove_after_keeps_boundary() {
        let mut manager = SubtitleManager::new();

        let mk_entry = |index: u32, start: &str| SubtitleEntry {
            index,
            start_time: Timestamp::parse(start).unwrap(),
            end_time: Timestamp::parse(start).unwrap(),
            text: "x".to_string(),
        };

        manager.add_entry(1000, mk_entry(1, "00:00:01,000"));
        manager.add_entry(2000, mk_entry(2, "00:00:02,000"));
        manager.add_entry(3000, mk_entry(3, "00:00:03,000"));

        manager.remove_after(2000);
        assert_eq!(manager.len(), 2);
        assert!(manager.entries.contains_key(&1000));
        assert!(manager.entries.contains_key(&2000));
        assert!(!manager.entries.contains_key(&3000));
    }
}
