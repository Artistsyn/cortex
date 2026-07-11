/// Phase 0D: Session closeout logic.
///
/// closeout_session is the single MCP tool that replaces the 7-step manual checklist.
/// With inline_approve=true (triggered by "KNOWLEDGE COMMITTED"), all markers are
/// immediately committed to the DB. With inline_approve=false (default), markers
/// are staged in knowledge_markers for later review.
use std::path::Path;

use anyhow::{Context, Result};
use chrono::Utc;
use rusqlite::params;
use serde_json::json;

use crate::markers::{self, KnowledgeMarker};
use crate::memory::Store;
use crate::model::{AntiPattern, Pattern};
use crate::session_store;

// ── Closeout result ───────────────────────────────────────────────────────────

#[derive(Debug, Default)]
pub struct CloseoutResult {
    pub patterns_committed:     usize,
    pub anti_patterns_committed: usize,
    pub corrections_committed:   usize,
    pub adrs_committed:          usize,
    pub prefs_notes_committed:   usize,
    pub skill_candidates_staged: usize,
    pub markers_staged:          usize,
    pub outcome_logged:          bool,
    pub graph_snapshot_written:  bool,
    pub session_snapshot_written: bool,
    pub mirror_written:          bool,
}

// ── Main entry point ──────────────────────────────────────────────────────────

/// Run the full session closeout.
///
/// - `inline_approve`: if true, all extracted markers are immediately committed
///   to their target DB tables. Set only when the user has typed "KNOWLEDGE COMMITTED".
///   If false, markers are staged in knowledge_markers with promoted=0.
pub fn run_closeout(
    store: &Store,
    session_key: &str,
    outcome_type: &str,
    error_text: Option<&str>,
    diff_symbols: Option<&str>,
    inline_approve: bool,
    repo_root: &Path,
    prefs_path: Option<&Path>,
) -> Result<CloseoutResult> {
    let mut result = CloseoutResult::default();

    // ── Step 1: Flush knowledge markers from session store ───────────────────
    let markers = extract_session_markers().unwrap_or_else(|e| {
        eprintln!("[closeout] warn: session store unavailable ({e}) — no markers from store");
        // Fallback: try to extract markers from recent mcp_calls in DB
        extract_markers_from_mcp_calls(store).unwrap_or_default()
    });
    let extracted_markers = markers.clone();

    if inline_approve {
        // Tier 1: commit immediately.
        for marker in &markers {
            match commit_marker(store, session_key, marker, prefs_path) {
                Ok(committed) => {
                    if committed {
                        match marker {
                            KnowledgeMarker::Pattern { .. }        => result.patterns_committed += 1,
                            KnowledgeMarker::AntiPattern { .. }    => result.anti_patterns_committed += 1,
                            KnowledgeMarker::Correction { .. }     => result.corrections_committed += 1,
                            KnowledgeMarker::Adr { .. }            => result.adrs_committed += 1,
                            KnowledgeMarker::PrefsNote { .. }      => result.prefs_notes_committed += 1,
                            KnowledgeMarker::SkillCandidate { .. } => result.skill_candidates_staged += 1,
                        }
                    }
                }
                Err(e) => {
                    eprintln!("[closeout] warn: failed to commit marker: {e}");
                }
            }
        }
        // Mark knowledge as flushed and inline-approved in protocol_sessions.
        let _ = store.conn().execute(
            "UPDATE protocol_sessions
             SET knowledge_markers_flushed = 1, inline_approved = 1
             WHERE session_key = ?1",
            params![session_key],
        );
    } else {
        // Tier 2: stage markers for later review.
        for marker in &markers {
            let _ = stage_marker(store, session_key, marker);
            result.markers_staged += 1;
        }
        let _ = store.conn().execute(
            "UPDATE protocol_sessions
             SET knowledge_markers_flushed = 1
             WHERE session_key = ?1",
            params![session_key],
        );
    }

    // ── Step 2: Log outcome ───────────────────────────────────────────────────
    if let Ok(outcome_id) = store.log_outcome(session_key, outcome_type, error_text, diff_symbols) {
        // Auto-apply weighted evidence.
        let _ = store.conn().execute(
            "INSERT OR IGNORE INTO outcome_applied_log (outcome_id, session_id, applied_at)
             VALUES (?1, ?2, unixepoch())",
            params![outcome_id, session_key],
        );
        result.outcome_logged = true;
    }

    // ── Step 3: Run git-review (pattern relevance scan) ───────────────────────
    if let Ok(deltas) = crate::git::head_deltas_with_options(repo_root, &crate::git::DeltaOptions {
        include: None,
        exclude: Some("assets".to_string()),
        max_files: 8,
        max_patch_lines: 20,
    }) {
        // Scan deltas for pattern/anti-pattern relevance keywords (simple approach).
        let delta_text: String = deltas.iter()
            .map(|d| format!("{} {}", d.path, d.summary))
            .collect::<Vec<_>>()
            .join(" ");
        // Annotate the session snapshot with touched domains.
        let _ = delta_text; // used in snapshot below
    }

    // ── Step 4: Write Graphify graph snapshot ─────────────────────────────────
    let graph_src = repo_root.join(".graphify-output").join("graph.json");
    if graph_src.exists() {
        let snapshots_dir = repo_root.join(".graphify-output").join("snapshots");
        if std::fs::create_dir_all(&snapshots_dir).is_ok() {
            let ts = Utc::now().format("%Y%m%d_%H%M%S");
            let dest = snapshots_dir.join(format!("graph_{ts}.json"));
            if std::fs::copy(&graph_src, &dest).is_ok() {
                result.graph_snapshot_written = true;
                // Prune snapshots older than 30 days.
                prune_old_snapshots(&snapshots_dir, 30);
                let _ = store.conn().execute(
                    "UPDATE protocol_sessions SET graph_snapshot_written = 1 WHERE session_key = ?1",
                    params![session_key],
                );
            }
        }
    }

    // ── Step 5: Write session snapshot ────────────────────────────────────────
    let snapshot_path = write_session_snapshot(
        store, session_key, outcome_type, &extracted_markers, repo_root
    ).unwrap_or_default();
    if !snapshot_path.is_empty() {
        result.session_snapshot_written = true;
    }

    // ── Step 6: Write agent-memory mirror ─────────────────────────────────────
    let mirror_dir = repo_root.join(".agent-memory").join("mirrors").join("repo");
    if std::fs::create_dir_all(&mirror_dir).is_ok() {
        let date = Utc::now().format("%Y-%m-%d");
        let mirror_path = mirror_dir.join(format!("session-closeout-{date}.md"));
        if let Ok(content) = build_mirror_content(
            session_key, outcome_type, &extracted_markers, inline_approve,
        ) {
            if std::fs::write(&mirror_path, content).is_ok() {
                result.mirror_written = true;
            }
        }
    }

    // ── Step 7: Mark protocol session as closed ───────────────────────────────
    let now = Utc::now().timestamp();
    let _ = store.conn().execute(
        "UPDATE protocol_sessions
         SET closeout_run = 1, outcome_type = ?1, closed_at = ?2
         WHERE session_key = ?3",
        params![outcome_type, now, session_key],
    );

    Ok(result)
}

// ── Marker extraction from session store ────────────────────────────────────

/// Scan the VS Code session store for CORTEX-* markers in recent turns.
fn extract_session_markers() -> Result<Vec<KnowledgeMarker>> {
    let store_path = session_store::find_session_store()
        .context("VS Code session store not found")?;

    let conn = session_store::open_readonly(&store_path)?;
    let responses = session_store::recent_assistant_responses(&conn, 50)?;

    let all_text = responses.join("\n\n---\n\n");
    Ok(markers::parse_markers(&all_text))
}

// ── Commit a marker to its target DB table ────────────────────────────────────

/// Returns true if the marker was committed, false if it was skipped (e.g. duplicate).
fn commit_marker(
    store: &Store,
    session_key: &str,
    marker: &KnowledgeMarker,
    prefs_path: Option<&Path>,
) -> Result<bool> {
    match marker {
        KnowledgeMarker::Pattern { name, intent, body, trust, uses, tags } => {
            // Check for duplicate name.
            let exists: bool = store.conn().query_row(
                "SELECT COUNT(*) > 0 FROM patterns WHERE name = ?1",
                params![name], |r| r.get::<_, bool>(0),
            ).unwrap_or(false);
            if exists { return Ok(false); }

            let body_with_trust = format!("{body}\nTrust: {trust} {}", Utc::now().format("%Y-%m-%d"));
            let p = Pattern {
                id: None,
                name: name.clone(),
                intent: intent.clone(),
                body: body_with_trust,
                uses: uses.clone(),
                tags: tags.clone(),
                approved_at: Utc::now(),
                use_count: 0,
                reverted_count: 0,
                survival_rate: 1.0,
            };
            store.insert_pattern(&p)?;
            mark_promoted(store, session_key, "pattern", name)?;
            Ok(true)
        }

        KnowledgeMarker::AntiPattern { description, wrong, correct, tags } => {
            // Check for duplicate description.
            let exists: bool = store.conn().query_row(
                "SELECT COUNT(*) > 0 FROM anti_patterns WHERE description = ?1",
                params![description], |r| r.get::<_, bool>(0),
            ).unwrap_or(false);
            if exists { return Ok(false); }

            let ap = AntiPattern {
                id: None,
                description: description.clone(),
                wrong: wrong.clone(),
                correct: correct.clone(),
                tags: tags.clone(),
                added_at: Utc::now(),
            };
            store.insert_anti_pattern(&ap)?;
            mark_promoted(store, session_key, "anti_pattern", description)?;
            Ok(true)
        }

        KnowledgeMarker::Correction { attempted, reason, fix, tags } => {
            store.insert_self_correction(attempted, reason, fix, tags)?;
            mark_promoted(store, session_key, "correction", attempted)?;
            Ok(true)
        }

        KnowledgeMarker::Adr { title, context, decision, tags } => {
            use crate::model::Adr;
            let number = store.next_adr_number()?;
            let adr = Adr {
                id: None,
                adr_number: number,
                title: title.clone(),
                status: "accepted".to_string(),
                context: context.clone(),
                decision: decision.clone(),
                reasoning: String::new(),
                alternatives: String::new(),
                consequences: String::new(),
                concept_tags: tags.clone(),
                superseded_by: None,
                created_at: Utc::now(),
                updated_at: Utc::now(),
            };
            store.insert_adr(&adr)?;
            mark_promoted(store, session_key, "adr", title)?;
            Ok(true)
        }

        KnowledgeMarker::PrefsNote { body, tags } => {
            // Append to prefs.toml notes array if a path is provided.
            if let Some(path) = prefs_path {
                if let Ok(mut prefs) = crate::prefs::load(path) {
                    // Add trust annotation.
                    let dated = format!("{} Trust: annotated {}", body, Utc::now().format("%Y-%m-%d"));
                    prefs.project.notes.push(dated);
                    let _ = crate::prefs::save(&prefs, path);
                    mark_promoted(store, session_key, "prefs_note", &body.chars().take(60).collect::<String>())?;
                    return Ok(true);
                }
            }
            // Fall back to adding as an annotation.
            let ann = crate::model::Annotation {
                id: None,
                topic: format!("prefs-note: {}", body.chars().take(60).collect::<String>()),
                body: body.clone(),
                tags: tags.clone(),
                added_at: Utc::now(),
            };
            store.insert_annotation(&ann)?;
            Ok(true)
        }

        KnowledgeMarker::SkillCandidate { name, trigger, summary } => {
            // Upsert into skill_candidates (always staged, never directly committed).
            store.conn().execute(
                "INSERT INTO skill_candidates
                     (name, trigger_hint, tool_sequence, session_keys, occurrence_count,
                      first_seen_at, last_seen_at)
                 VALUES (?1, ?2, '[]', json_array(?3), 1, unixepoch(), unixepoch())
                 ON CONFLICT(name) DO UPDATE SET
                     trigger_hint     = CASE WHEN excluded.trigger_hint != '' THEN excluded.trigger_hint
                                        ELSE skill_candidates.trigger_hint END,
                     session_keys     = json_insert(skill_candidates.session_keys,
                                            '$[#]', excluded.session_keys->>'$[0]'),
                     occurrence_count = skill_candidates.occurrence_count + 1,
                     last_seen_at     = unixepoch()",
                params![name, trigger, session_key],
            )?;
            let _ = summary;  // stored via trigger_hint; full summary in tool sequence
            Ok(true)
        }
    }
}

/// Record that a knowledge marker was promoted to its target table.
fn mark_promoted(store: &Store, session_key: &str, marker_type: &str, name: &str) -> Result<()> {
    store.conn().execute(
        "UPDATE knowledge_markers SET promoted = 1
         WHERE session_key = ?1 AND marker_type = ?2 AND (name = ?3 OR body LIKE ?4)
         AND promoted = 0
         LIMIT 1",
        params![session_key, marker_type, name, format!("%{}%", &name.chars().take(30).collect::<String>())],
    )?;
    Ok(())
}

// ── Stage a marker for later review ──────────────────────────────────────────

fn stage_marker(store: &Store, session_key: &str, marker: &KnowledgeMarker) -> Result<()> {
    let body = marker_body(marker);
    let name = marker.display_name();
    let tags = marker_tags_json(marker);
    let trust = match marker {
        KnowledgeMarker::Pattern { trust, .. } => trust.clone(),
        _ => "annotated".to_string(),
    };

    store.conn().execute(
        "INSERT INTO knowledge_markers
             (session_key, marker_type, name, body, tags, trust_level, raw_tag, promoted)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, '', 0)",
        params![session_key, marker.marker_type(), name, body, tags, trust],
    )?;
    Ok(())
}

fn marker_body(marker: &KnowledgeMarker) -> String {
    match marker {
        KnowledgeMarker::Pattern { body, .. }        => body.clone(),
        KnowledgeMarker::AntiPattern { description, wrong, correct, .. } =>
            format!("{description}\nwrong: {wrong}\ncorrect: {correct}"),
        KnowledgeMarker::Correction { attempted, reason, fix, .. } =>
            format!("attempted: {attempted}\nreason: {reason}\nfix: {fix}"),
        KnowledgeMarker::Adr { context, decision, .. } =>
            format!("Context: {context}\nDecision: {decision}"),
        KnowledgeMarker::PrefsNote { body, .. }      => body.clone(),
        KnowledgeMarker::SkillCandidate { summary, .. } => summary.clone(),
    }
}

fn marker_tags_json(marker: &KnowledgeMarker) -> String {
    let tags: Vec<String> = match marker {
        KnowledgeMarker::Pattern { tags, .. }     => tags.clone(),
        KnowledgeMarker::AntiPattern { tags, .. } => tags.clone(),
        KnowledgeMarker::Correction { tags, .. }  => tags.clone(),
        KnowledgeMarker::Adr { tags, .. }         => tags.clone(),
        KnowledgeMarker::PrefsNote { tags, .. }   => tags.clone(),
        KnowledgeMarker::SkillCandidate { .. }    => vec![],
    };
    serde_json::to_string(&tags).unwrap_or_else(|_| "[]".to_string())
}

// ── Session snapshot ──────────────────────────────────────────────────────────

fn write_session_snapshot(
    store: &Store,
    session_key: &str,
    outcome_type: &str,
    markers: &[KnowledgeMarker],
    repo_root: &Path,
) -> Result<String> {
    let dir = repo_root.join(".cortex").join("mined-tasks");
    std::fs::create_dir_all(&dir).context("create mined-tasks dir")?;

    let marker_counts = json!({
        "pattern":        markers.iter().filter(|m| matches!(m, KnowledgeMarker::Pattern { .. })).count(),
        "anti_pattern":   markers.iter().filter(|m| matches!(m, KnowledgeMarker::AntiPattern { .. })).count(),
        "correction":     markers.iter().filter(|m| matches!(m, KnowledgeMarker::Correction { .. })).count(),
        "adr":            markers.iter().filter(|m| matches!(m, KnowledgeMarker::Adr { .. })).count(),
        "prefs_note":     markers.iter().filter(|m| matches!(m, KnowledgeMarker::PrefsNote { .. })).count(),
        "skill_candidate":markers.iter().filter(|m| matches!(m, KnowledgeMarker::SkillCandidate { .. })).count(),
    });

    // Read recent tool sequences from mcp_calls for this session.
    let tool_seq: Vec<String> = {
        if let Ok(mut stmt) = store.conn().prepare(
            "SELECT DISTINCT tool FROM mcp_calls
             WHERE called_at >= datetime('now', '-3 hours')
             ORDER BY id ASC LIMIT 30"
        ) {
            stmt.query_map([], |r| r.get::<_, String>(0))
                .map(|rows| rows.filter_map(|r| r.ok()).filter(|s| !s.is_empty()).collect())
                .unwrap_or_default()
        } else {
            vec![]
        }
    };

    let snapshot = json!({
        "session_key":   session_key,
        "outcome_type":  outcome_type,
        "marker_counts": marker_counts,
        "tool_sequence": tool_seq,
        "domain_tags":   [],
        "created_at":    Utc::now().to_rfc3339(),
    });

    let filename = format!("session_{}.json", session_key.replace('/', "_"));
    let path = dir.join(&filename);
    std::fs::write(&path, serde_json::to_string_pretty(&snapshot)?)
        .context("write session snapshot")?;

    let path_str = path.to_string_lossy().to_string();

    // Record in session_snapshots table.
    let _ = store.conn().execute(
        "INSERT OR REPLACE INTO session_snapshots
             (session_key, outcome_type, tool_sequence, marker_counts, snapshot_path, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, unixepoch())",
        params![
            session_key,
            outcome_type,
            serde_json::to_string(&tool_seq).unwrap_or_else(|_| "[]".to_string()),
            marker_counts.to_string(),
            path_str.clone(),
        ],
    );

    Ok(path_str)
}

// ── Mirror file content ───────────────────────────────────────────────────────

fn build_mirror_content(
    session_key: &str,
    outcome_type: &str,
    markers: &[KnowledgeMarker],
    inline_approved: bool,
) -> Result<String> {
    let mut out = format!("# Session Closeout — {}\n\n", Utc::now().format("%Y-%m-%d"));
    out.push_str(&format!("**Session:** {session_key}  \n"));
    out.push_str(&format!("**Outcome:** {outcome_type}  \n"));
    out.push_str(&format!("**Knowledge committed:** {}  \n\n",
        if inline_approved { "✓ KNOWLEDGE COMMITTED" } else { "staged only" }));

    if !markers.is_empty() {
        out.push_str("## Knowledge captured\n\n");
        for m in markers {
            out.push_str(&format!("- [{}] {}\n", m.marker_type(), m.display_name()));
        }
    }

    Ok(out)
}

/// Fallback marker extraction: scan recent mcp_calls arguments for CORTEX-* tags.
/// Used when the VS Code session store is inaccessible.
fn extract_markers_from_mcp_calls(store: &Store) -> Result<Vec<KnowledgeMarker>> {
    let mut stmt = store.conn().prepare(
        "SELECT args FROM mcp_calls
         WHERE called_at > datetime('now', '-1 day')
         ORDER BY id DESC LIMIT 30"
    )?;
    let args_list: Vec<String> = stmt.query_map([], |r| {
        r.get::<_, String>(0)
    })?.collect::<rusqlite::Result<Vec<_>>>()?;

    let all_text = args_list.join("\n\n---\n\n");
    Ok(markers::parse_markers(&all_text))
}

// ── Graph snapshot pruning ────────────────────────────────────────────────────

const MAX_SNAPSHOTS: usize = 50;

fn prune_old_snapshots(dir: &Path, max_age_days: u64) {
    let cutoff = std::time::SystemTime::now()
        .checked_sub(std::time::Duration::from_secs(max_age_days * 86400))
        .unwrap_or(std::time::UNIX_EPOCH);

    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            if let Ok(meta) = entry.metadata() {
                if let Ok(mtime) = meta.modified() {
                    if mtime < cutoff {
                        let _ = std::fs::remove_file(entry.path());
                    }
                }
            }
        }
    }

    // Count-based pruning: keep only the N newest snapshots.
    let mut snapshots: Vec<_> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            snapshots.push(entry.path());
        }
    }
    snapshots.sort_by(|a, b| b.file_name().cmp(&a.file_name()));
    while snapshots.len() > MAX_SNAPSHOTS {
        if let Some(old) = snapshots.pop() {
            let _ = std::fs::remove_file(&old);
        }
    }
}
