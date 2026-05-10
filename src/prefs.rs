use std::path::Path;

use anyhow::Result;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Preferences {
    #[serde(default)]
    pub style: StylePrefs,
    #[serde(default)]
    pub patterns: PatternPrefs,
    #[serde(default)]
    pub api: ApiPrefs,
    #[serde(default)]
    pub project: ProjectPrefs,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct StylePrefs {
    #[serde(default)]
    pub line_length: u32,
    #[serde(default)]
    pub indent: String,
    #[serde(default)]
    pub naming: String,
    #[serde(default)]
    pub error_handling: String,
    #[serde(default)]
    pub comments: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PatternPrefs {
    #[serde(default)]
    pub preferred: Vec<String>,
    #[serde(default)]
    pub avoid: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ApiPrefs {
    #[serde(default)]
    pub primary_building_blocks: Vec<String>,
    #[serde(default)]
    pub never_raw: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProjectPrefs {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub language: String,
    #[serde(default)]
    pub min_rust: String,
    #[serde(default)]
    pub notes: Vec<String>,
}

pub fn load(path: &Path) -> Result<Preferences> {
    if !path.exists() {
        return Ok(Preferences::default());
    }
    let src = std::fs::read_to_string(path)?;
    let prefs: Preferences = toml::from_str(&src)?;
    Ok(prefs)
}

pub fn save(prefs: &Preferences, path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let src = toml::to_string_pretty(prefs)?;
    std::fs::write(path, src)?;
    Ok(())
}

pub fn render_for_copilot(prefs: &Preferences) -> String {
    let mut out = String::new();
    out.push_str("=== PREFERENCES ===\n");

    if prefs.style.line_length > 0 {
        out.push_str(&format!("line_length: {}\n", prefs.style.line_length));
    }
    if !prefs.style.indent.is_empty() {
        out.push_str(&format!("indent: {}\n", prefs.style.indent));
    }
    if !prefs.style.naming.is_empty() {
        out.push_str(&format!("naming: {}\n", prefs.style.naming));
    }
    if !prefs.style.error_handling.is_empty() {
        out.push_str(&format!("error_handling: {}\n", prefs.style.error_handling));
    }
    if !prefs.style.comments.is_empty() {
        out.push_str(&format!("comments: {}\n", prefs.style.comments));
    }

    if !prefs.patterns.preferred.is_empty() {
        out.push_str(&format!("preferred_patterns: {}\n", prefs.patterns.preferred.join(", ")));
    }
    if !prefs.patterns.avoid.is_empty() {
        out.push_str(&format!("avoid_patterns: {}\n", prefs.patterns.avoid.join(", ")));
    }

    if !prefs.api.primary_building_blocks.is_empty() {
        out.push_str(&format!("primary_api: {}\n", prefs.api.primary_building_blocks.join(", ")));
    }
    if !prefs.api.never_raw.is_empty() {
        out.push_str(&format!("never_raw: {}\n", prefs.api.never_raw.join(", ")));
    }

    if !prefs.project.name.is_empty() {
        out.push_str(&format!("project: {}\n", prefs.project.name));
    }
    if !prefs.project.language.is_empty() {
        out.push_str(&format!("language: {}\n", prefs.project.language));
    }
    if !prefs.project.min_rust.is_empty() {
        out.push_str(&format!("min_rust: {}\n", prefs.project.min_rust));
    }
    if !prefs.project.notes.is_empty() {
        out.push_str(&format!("notes: {}\n", prefs.project.notes.join(" | ")));
    }

    out
}
