//! Psychographic profile — auto-built user model from conversation signals.
//!
//! Combines IronClaw-style broad fields with keeper.md alignment.
//! Two extraction methods:
//! - Inline regex: cheap pattern matching every turn for obvious signals
//! - Heartbeat LLM: periodic deep extraction for nuanced signals
//!
//! Profile is tiered:
//! - Tier 1 (confidence > 0.3): interaction style summary injected into prompt
//! - Tier 2 (confidence > 0.6, updated within 7 days): full personalization

pub mod extract;

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::{FamiliarError, Result};

/// Psychographic profile for the operator.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Profile {
    /// Overall confidence in the profile (0.0 - 1.0).
    pub confidence: f32,
    /// When the profile was last updated.
    pub updated_at: Option<DateTime<Utc>>,
    /// Number of conversations that contributed to this profile.
    pub conversation_count: u32,

    // IronClaw-broad fields
    pub profession: Option<ProfileField>,
    pub communication_style: Option<ProfileField>,
    pub goals: Option<ProfileField>,
    pub frustrations: Option<ProfileField>,
    pub time_patterns: Option<ProfileField>,
    pub expertise_areas: Option<ProfileField>,
    pub preferences: Option<ProfileField>,

    // Keeper-aligned fields
    pub working_style: Option<ProfileField>,
    pub philosophy: Option<ProfileField>,
    pub creative_preferences: Option<ProfileField>,
}

/// A single profile field with value and per-field confidence.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfileField {
    pub value: String,
    pub confidence: f32,
    pub source: String, // "inline" or "llm"
    pub updated_at: DateTime<Utc>,
}

impl Profile {
    /// Load profile from disk, or return default if not found.
    pub fn load(path: &Path) -> Self {
        match std::fs::read_to_string(path) {
            Ok(content) => serde_json::from_str(&content).unwrap_or_default(),
            Err(_) => Self::default(),
        }
    }

    /// Save profile to disk.
    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| FamiliarError::Internal {
                reason: format!("failed to create profile directory: {}", e),
            })?;
        }

        let json = serde_json::to_string_pretty(self).map_err(|e| FamiliarError::Internal {
            reason: format!("failed to serialize profile: {}", e),
        })?;

        std::fs::write(path, json).map_err(|e| FamiliarError::Internal {
            reason: format!("failed to write profile: {}", e),
        })?;

        Ok(())
    }

    /// Update a field, recalculating overall confidence.
    pub fn set_field(&mut self, name: &str, value: String, confidence: f32, source: &str) {
        let field = ProfileField {
            value,
            confidence,
            source: source.to_string(),
            updated_at: Utc::now(),
        };

        match name {
            "profession" => self.profession = Some(field),
            "communication_style" => self.communication_style = Some(field),
            "goals" => self.goals = Some(field),
            "frustrations" => self.frustrations = Some(field),
            "time_patterns" => self.time_patterns = Some(field),
            "expertise_areas" => self.expertise_areas = Some(field),
            "preferences" => self.preferences = Some(field),
            "working_style" => self.working_style = Some(field),
            "philosophy" => self.philosophy = Some(field),
            "creative_preferences" => self.creative_preferences = Some(field),
            _ => return,
        }

        self.updated_at = Some(Utc::now());
        self.recalculate_confidence();
    }

    /// Recalculate overall confidence from field confidences.
    fn recalculate_confidence(&mut self) {
        let fields = self.all_fields();
        if fields.is_empty() {
            self.confidence = 0.0;
            return;
        }
        let sum: f32 = fields.iter().map(|f| f.confidence).sum();
        self.confidence = sum / fields.len() as f32;
    }

    /// Get all populated fields.
    fn all_fields(&self) -> Vec<&ProfileField> {
        let mut fields = Vec::new();
        if let Some(ref f) = self.profession { fields.push(f); }
        if let Some(ref f) = self.communication_style { fields.push(f); }
        if let Some(ref f) = self.goals { fields.push(f); }
        if let Some(ref f) = self.frustrations { fields.push(f); }
        if let Some(ref f) = self.time_patterns { fields.push(f); }
        if let Some(ref f) = self.expertise_areas { fields.push(f); }
        if let Some(ref f) = self.preferences { fields.push(f); }
        if let Some(ref f) = self.working_style { fields.push(f); }
        if let Some(ref f) = self.philosophy { fields.push(f); }
        if let Some(ref f) = self.creative_preferences { fields.push(f); }
        fields
    }

    /// Generate Tier 1 prompt injection (interaction style summary).
    /// Injected when confidence > 0.3.
    pub fn tier1_prompt(&self) -> Option<String> {
        if self.confidence < 0.3 {
            return None;
        }

        let mut parts = Vec::new();
        if let Some(ref f) = self.communication_style {
            parts.push(format!("Communication style: {}", f.value));
        }
        if let Some(ref f) = self.working_style {
            parts.push(format!("Working style: {}", f.value));
        }
        if let Some(ref f) = self.time_patterns {
            parts.push(format!("Active hours: {}", f.value));
        }

        if parts.is_empty() {
            return None;
        }

        Some(format!("## Operator Profile (Tier 1)\n\n{}", parts.join("\n")))
    }

    /// Generate Tier 2 prompt injection (full personalization).
    /// Injected when confidence > 0.6 and updated within 7 days.
    pub fn tier2_prompt(&self) -> Option<String> {
        if self.confidence < 0.6 {
            return None;
        }

        // Check freshness
        if let Some(updated) = self.updated_at {
            let age = Utc::now() - updated;
            if age.num_days() > 7 {
                return None;
            }
        } else {
            return None;
        }

        let mut parts = Vec::new();
        for (name, field) in [
            ("Profession", &self.profession),
            ("Communication style", &self.communication_style),
            ("Goals", &self.goals),
            ("Frustrations", &self.frustrations),
            ("Active hours", &self.time_patterns),
            ("Expertise", &self.expertise_areas),
            ("Preferences", &self.preferences),
            ("Working style", &self.working_style),
            ("Philosophy", &self.philosophy),
            ("Creative preferences", &self.creative_preferences),
        ] {
            if let Some(f) = field {
                parts.push(format!("- **{}**: {}", name, f.value));
            }
        }

        if parts.is_empty() {
            return None;
        }

        Some(format!(
            "## Operator Profile (Tier 2)\n\n{}\n\n*Profile confidence: {:.0}%*",
            parts.join("\n"),
            self.confidence * 100.0
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_profile_has_zero_confidence() {
        let p = Profile::default();
        assert_eq!(p.confidence, 0.0);
        assert!(p.tier1_prompt().is_none());
        assert!(p.tier2_prompt().is_none());
    }

    #[test]
    fn tier1_at_03() {
        let mut p = Profile::default();
        p.set_field("communication_style", "Direct, concise".into(), 0.5, "inline");
        p.set_field("working_style", "Action-oriented".into(), 0.4, "inline");
        // confidence = (0.5 + 0.4) / 2 = 0.45 > 0.3
        assert!(p.tier1_prompt().is_some());
        assert!(p.tier2_prompt().is_none()); // < 0.6
    }

    #[test]
    fn tier2_at_06_and_fresh() {
        let mut p = Profile::default();
        for field in &["profession", "communication_style", "goals", "working_style", "expertise_areas"] {
            p.set_field(field, "test value".into(), 0.7, "llm");
        }
        // confidence = 0.7 (all fields same), updated_at = now
        assert!(p.tier1_prompt().is_some());
        assert!(p.tier2_prompt().is_some());
    }

    #[test]
    fn save_and_load_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("profile.json");

        let mut p = Profile::default();
        p.set_field("profession", "Engineer".into(), 0.8, "inline");
        p.save(&path).unwrap();

        let loaded = Profile::load(&path);
        assert_eq!(loaded.profession.unwrap().value, "Engineer");
    }
}
