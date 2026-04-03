//! Inline profile extraction — cheap regex-based signal detection.
//!
//! Scans user messages for obvious profile signals. Zero LLM cost.
//! Deeper extraction happens in the heartbeat LLM pass.

use regex::Regex;
use std::sync::OnceLock;

/// Extracted signal from a user message.
#[derive(Debug)]
pub struct Signal {
    pub field: &'static str,
    pub value: String,
    pub confidence: f32,
}

/// Scan a user message for profile signals. Returns zero or more signals.
pub fn extract_signals(message: &str) -> Vec<Signal> {
    let mut signals = Vec::new();

    // Profession detection
    static PROFESSION_RE: OnceLock<Regex> = OnceLock::new();
    let prof_re = PROFESSION_RE.get_or_init(|| {
        Regex::new(r"(?i)\b(?:i'm|i am|i work as|my role is|my job is)\s+(?:a\s+|an\s+)?([^.,!?]+)")
            .unwrap()
    });
    if let Some(cap) = prof_re.captures(message) {
        if let Some(m) = cap.get(1) {
            signals.push(Signal {
                field: "profession",
                value: m.as_str().trim().to_string(),
                confidence: 0.6,
            });
        }
    }

    // Communication style from message characteristics
    let word_count = message.split_whitespace().count();
    if word_count <= 5 {
        signals.push(Signal {
            field: "communication_style",
            value: "Terse, direct".to_string(),
            confidence: 0.3,
        });
    }

    // Expertise detection
    static EXPERTISE_RE: OnceLock<Regex> = OnceLock::new();
    let exp_re = EXPERTISE_RE.get_or_init(|| {
        Regex::new(r"(?i)\b(?:i(?:'ve| have) (?:been|worked)(?: with| in| on)?\s+(.+?)(?:\s+for\s+\d+\s+years?)?[.,!?]|my expertise is\s+(.+?)[.,!?])").unwrap()
    });
    if let Some(cap) = exp_re.captures(message) {
        let value = cap
            .get(1)
            .or(cap.get(2))
            .map(|m| m.as_str().trim().to_string());
        if let Some(v) = value {
            signals.push(Signal {
                field: "expertise_areas",
                value: v,
                confidence: 0.5,
            });
        }
    }

    // Time pattern from message timestamp (structural signal)
    let hour = chrono::Local::now().hour();
    if hour >= 22 || hour < 6 {
        signals.push(Signal {
            field: "time_patterns",
            value: "Night owl (active late evening/early morning)".to_string(),
            confidence: 0.2, // Low — single observation
        });
    }

    // Preference detection
    static PREF_RE: OnceLock<Regex> = OnceLock::new();
    let pref_re = PREF_RE.get_or_init(|| {
        Regex::new(r"(?i)\b(?:i prefer|i like|i want|keep it)\s+(.+?)(?:[.,!?]|$)").unwrap()
    });
    if let Some(cap) = pref_re.captures(message) {
        if let Some(m) = cap.get(1) {
            let value = m.as_str().trim().to_string();
            if value.len() > 3 && value.len() < 100 {
                signals.push(Signal {
                    field: "preferences",
                    value,
                    confidence: 0.4,
                });
            }
        }
    }

    signals
}

use chrono::Timelike;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_profession() {
        let signals = extract_signals("I'm a data scientist investigating logging");
        assert!(signals.iter().any(|s| s.field == "profession"));
    }

    #[test]
    fn detects_preference() {
        let signals = extract_signals("I prefer dark mode and concise responses.");
        assert!(signals.iter().any(|s| s.field == "preferences"));
    }

    #[test]
    fn terse_message_detected() {
        let signals = extract_signals("fix it");
        assert!(signals
            .iter()
            .any(|s| s.field == "communication_style" && s.value.contains("Terse")));
    }

    #[test]
    fn no_false_positives_on_normal() {
        let signals = extract_signals("Can you help me refactor this function?");
        // Should not detect profession or expertise from a normal request
        assert!(!signals.iter().any(|s| s.field == "profession"));
    }
}
