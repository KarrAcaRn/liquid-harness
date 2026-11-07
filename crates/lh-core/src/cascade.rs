//! Deterministic routing helpers. Hints are literal Unicode word phrases, not regex.
use lh_protocol::{RouteDecision, RouteInput, Specialist};
use serde::{Deserialize, Serialize};
use std::{
    collections::VecDeque,
    hash::{DefaultHasher, Hash, Hasher},
    io::Read,
};

/// Caller-owned conversation state. Failed routes never advance it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Session {
    pub previous_specialist: Option<String>,
    pub previous_input: Option<RouteInput>,
}
impl Session {
    pub fn reset(&mut self) {
        *self = Self::default();
    }
    pub(crate) fn remember(&mut self, input: &RouteInput, decision: &RouteDecision) {
        self.previous_specialist = Some(decision.specialist.clone());
        self.previous_input = Some(input.clone());
    }
    pub(crate) fn same_modality(&self, input: &RouteInput) -> bool {
        self.previous_input.as_ref().is_some_and(|p| {
            p.images.is_empty() == input.images.is_empty()
                && p.audio.is_empty() == input.audio.is_empty()
        })
    }
}

pub(crate) fn requirements(s: &Specialist, input: &RouteInput) -> bool {
    s.requires.iter().all(|m| match m.as_str() {
        "text" => !input.prompt.trim().is_empty(),
        "image" => !input.images.is_empty(),
        "audio" => !input.audio.is_empty(),
        _ => false,
    })
}
fn words(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric() && c != '_')
        .filter(|s| !s.is_empty())
        .map(str::to_lowercase)
        .collect()
}
/// Whole contiguous word matching; punctuation separates words. No regex syntax.
pub fn phrase_matches(prompt: &str, pattern: &str) -> bool {
    let prompt = words(prompt);
    let pattern = words(pattern);
    !pattern.is_empty() && prompt.windows(pattern.len()).any(|w| w == pattern)
}
pub(crate) fn hint_score(s: &Specialist, prompt: &str) -> f64 {
    s.hints
        .iter()
        .filter(|h| phrase_matches(prompt, &h.pattern))
        .map(|h| h.weight)
        .sum()
}

/// Unreadable files and mutable remote URLs cannot safely be cached.
pub(crate) fn attachment_fingerprints(input: &RouteInput) -> Option<Vec<u64>> {
    input
        .images
        .iter()
        .chain(&input.audio)
        .map(|reference| {
            let mut h = DefaultHasher::new();
            reference.hash(&mut h);
            if reference.starts_with("data:") {
                return Some(h.finish());
            }
            let path = reference.strip_prefix("file:").unwrap_or(reference);
            if !std::fs::metadata(path).ok()?.is_file() {
                return None;
            }
            let mut file = std::fs::File::open(path).ok()?;
            if !file.metadata().ok()?.is_file() {
                return None;
            }
            let mut buffer = [0u8; 8192];
            loop {
                let n = file.read(&mut buffer).ok()?;
                if n == 0 {
                    break;
                }
                h.write(&buffer[..n]);
            }
            Some(h.finish())
        })
        .collect()
}

/// Bounded LRU with a hash accelerator and exact key equality for collisions.
#[derive(Default)]
pub(crate) struct RouteCache {
    entries: VecDeque<(u64, String, RouteDecision)>,
}
fn fingerprint(key: &str) -> u64 {
    let mut h = DefaultHasher::new();
    key.hash(&mut h);
    h.finish()
}
impl RouteCache {
    pub fn resize(&mut self, capacity: usize) {
        while self.entries.len() > capacity {
            self.entries.pop_front();
        }
    }
    pub fn get(&mut self, key: &str) -> Option<RouteDecision> {
        let hash = fingerprint(key);
        let i = self
            .entries
            .iter()
            .position(|(h, k, _)| *h == hash && k == key)?;
        let entry = self.entries.remove(i)?;
        let decision = entry.2.clone();
        self.entries.push_back(entry);
        Some(decision)
    }
    pub fn insert(&mut self, key: String, decision: RouteDecision, capacity: usize) {
        self.resize(capacity);
        if capacity == 0 {
            return;
        }
        self.entries.retain(|(_, k, _)| k != &key);
        self.entries.push_back((fingerprint(&key), key, decision));
        self.resize(capacity);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn literal_multilingual_word_phrases() {
        assert!(phrase_matches("Bitte LAUT vorlesen!", "laut vorlesen"));
        assert!(phrase_matches("Lis à voix haute.", "à voix haute"));
        assert!(!phrase_matches("outloudly", "loud"));
        assert!(!phrase_matches("read this aloud", "read aloud"));
        assert!(!phrase_matches("anything", ""));
    }
    fn decision() -> RouteDecision {
        lh_protocol::json::from_value(lh_protocol::json::json!({
            "specialist":"text", "confidence":1.0, "reason":"test"
        }))
        .unwrap()
    }
    #[test]
    fn lru_capacity_collision_and_case() {
        let mut c = RouteCache::default();
        c.insert("A".into(), decision(), 2);
        c.insert("B".into(), decision(), 2);
        assert!(c.get("A").is_some());
        assert!(c.get("a").is_none());
        c.insert("C".into(), decision(), 2);
        assert!(c.get("B").is_none());
        // Simulate a hash collision: unequal content must not hit.
        c.entries
            .push_back((fingerprint("different"), "original".into(), decision()));
        assert!(c.get("different").is_none());
        c.resize(0);
        assert!(c.entries.is_empty());
    }
}
