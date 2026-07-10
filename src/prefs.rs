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
    // Phase 0A: self-learning loop configuration sections.
    #[serde(default)]
    pub enforcement: EnforcementPrefs,
    #[serde(default)]
    pub consolidation: ConsolidationPrefs,
    #[serde(default)]
    pub skills: SkillsPrefs,
    #[serde(default)]
    pub memory: MemoryPrefs,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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

impl Default for StylePrefs {
    fn default() -> Self {
        Self {
            line_length: 0,
            indent: String::new(),
            naming: String::new(),
            error_handling: String::new(),
            comments: String::new(),
        }
    }
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

/// Protocol enforcement configuration (Phase 0A).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnforcementPrefs {
    /// "protocol_session_only" (default) or "always".
    /// "protocol_session_only": Phase 0 gating only when begin_protocol_session was called.
    /// "always": gate all work tool calls in every session.
    #[serde(default = "default_protocol_gate_mode")]
    pub protocol_gate_mode: String,
    /// Warn in get_context if previous session has no closeout record.
    #[serde(default = "default_true")]
    pub closeout_warning_enabled: bool,
    /// Hours before a session is considered orphaned without closeout.
    #[serde(default = "default_closeout_grace_hours")]
    pub closeout_grace_period_hours: u32,
}

impl Default for EnforcementPrefs {
    fn default() -> Self {
        Self {
            protocol_gate_mode: default_protocol_gate_mode(),
            closeout_warning_enabled: true,
            closeout_grace_period_hours: default_closeout_grace_hours(),
        }
    }
}

fn default_protocol_gate_mode() -> String { "protocol_session_only".to_string() }
fn default_closeout_grace_hours() -> u32 { 2 }
fn default_true() -> bool { true }

/// Consolidation pipeline configuration (Phase 0A).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConsolidationPrefs {
    /// Hours of staleness before consolidation runs at VS Code open.
    #[serde(default = "default_staleness_hours")]
    pub staleness_hours: u32,
    /// Maximum proposals committed per consolidation run.
    #[serde(default = "default_max_commits")]
    pub max_commits_per_run: u32,
    /// Minimum sessions in a cluster before generating proposals.
    #[serde(default = "default_min_cluster_sessions")]
    pub min_cluster_sessions: u32,
    /// Minimum occurrences before a skill candidate is drafted.
    #[serde(default = "default_skill_candidate_min")]
    pub skill_candidate_min_occurrences: u32,
    /// Graph snapshot retention days.
    #[serde(default = "default_snapshot_days")]
    pub graph_snapshot_days: u32,
}

impl Default for ConsolidationPrefs {
    fn default() -> Self {
        Self {
            staleness_hours: default_staleness_hours(),
            max_commits_per_run: default_max_commits(),
            min_cluster_sessions: default_min_cluster_sessions(),
            skill_candidate_min_occurrences: default_skill_candidate_min(),
            graph_snapshot_days: default_snapshot_days(),
        }
    }
}

fn default_staleness_hours() -> u32 { 8 }
fn default_max_commits() -> u32 { 5 }
fn default_min_cluster_sessions() -> u32 { 3 }
fn default_skill_candidate_min() -> u32 { 3 }
fn default_snapshot_days() -> u32 { 30 }

/// Skill self-authoring configuration (Phase 0A).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillsPrefs {
    /// Directory for skill files relative to workspace root.
    #[serde(default = "default_skills_dir")]
    pub skills_dir: String,
    /// Auto-update skill files when revision proposals are approved.
    #[serde(default = "default_true")]
    pub auto_update_skills: bool,
}

impl Default for SkillsPrefs {
    fn default() -> Self {
        Self {
            skills_dir: default_skills_dir(),
            auto_update_skills: true,
        }
    }
}

fn default_skills_dir() -> String { "agent_customization/skills".to_string() }

/// Memory file lifecycle configuration (Phase 0A).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryPrefs {
    /// Maximum agent memory mirror files before consolidation is recommended.
    #[serde(default = "default_max_mirror_files")]
    pub max_mirror_files: u32,
    /// Similarity threshold for mirror consolidation proposals (0.0–1.0).
    #[serde(default = "default_consolidation_threshold")]
    pub mirror_consolidation_threshold: f32,
}

impl Default for MemoryPrefs {
    fn default() -> Self {
        Self {
            max_mirror_files: default_max_mirror_files(),
            mirror_consolidation_threshold: default_consolidation_threshold(),
        }
    }
}

fn default_max_mirror_files() -> u32 { 200 }
fn default_consolidation_threshold() -> f32 { 0.75 }

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
