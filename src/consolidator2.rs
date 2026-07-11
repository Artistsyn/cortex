/// Phase 1: Consolidation pipeline orchestrator.
///
/// Runs the 6-stage pipeline that feeds the nightly consolidation:
///   1. health-check   → .cortex/health-report.json
///   2. cluster-sessions → .cortex/clusters.json
///   3. detect-skills  → .cortex/proposals/skill_*.md
///   4. propose-gaps   → .cortex/proposals/pref_gap_*.json
///   5. propose-survival → .cortex/proposals/ap_*.json (dying patterns)
///   6. propose-instructions → (stub for Phase 2)
///
/// Each stage is idempotent — safe to re-run.
use std::path::Path;

use anyhow::Result;
use chrono::Utc;
use serde_json::{json, Value};

use crate::memory::Store;
use crate::miner::{self, SessionCluster};
use crate::prefs::Preferences;
use crate::skills;

// ── Pipeline result ───────────────────────────────────────────────────────────

#[derive(Debug, Default)]
pub struct PipelineResult {
    pub snapshots_read:       usize,
    pub clusters_found:       usize,
    pub skill_candidates_new: usize,
    pub skill_drafts_written: usize,
    pub gap_proposals:        usize,
    pub survival_proposals:   usize,
    pub last_run_updated:     bool,
}

impl PipelineResult {
    pub fn summary(&self) -> String {
        format!(
            "Pipeline complete: {} snapshots → {} clusters → {} skill candidates ({} drafts) | {} gap proposals | {} survival proposals",
            self.snapshots_read,
            self.clusters_found,
            self.skill_candidates_new,
            self.skill_drafts_written,
            self.gap_proposals,
            self.survival_proposals,
        )
    }
}

// ── Main pipeline entry ───────────────────────────────────────────────────────

pub fn run(
    store: &Store,
    repo_root: &Path,
    prefs: &Preferences,
) -> Result<PipelineResult> {
    let mut result = PipelineResult::default();

    let mined_tasks_dir = repo_root.join(".cortex").join("mined-tasks");
    let proposals_dir   = repo_root.join(".cortex").join("proposals");
    let clusters_path   = repo_root.join(".cortex").join("clusters.json");
    let health_path     = repo_root.join(".cortex").join("health-report.json");

    std::fs::create_dir_all(&proposals_dir)?;

    // ── Stage 1: Health report ────────────────────────────────────────────────
    let health = build_health_report(store)?;
    std::fs::write(&health_path, serde_json::to_string_pretty(&health)?)?;

    // ── Stage 2: Cluster sessions ─────────────────────────────────────────────
    let snapshots = miner::load_snapshots(&mined_tasks_dir)?;
    result.snapshots_read = snapshots.len();

    let threshold   = 0.55f32;  // lower than Plan to avoid over-splitting sparse data
    let clusters    = miner::cluster_snapshots(&snapshots, threshold);
    result.clusters_found = clusters.len();

    std::fs::write(&clusters_path, miner::clusters_to_json(&clusters))?;

    // ── Stage 3: Detect skill candidates ─────────────────────────────────────
    let min_occ = prefs.consolidation.skill_candidate_min_occurrences as usize;
    let candidates = skills::detect_skill_candidates(store, &clusters, min_occ as u32)?;
    result.skill_candidates_new = candidates.len();

    for candidate in &candidates {
        match skills::draft_skill_file(
            &candidate.name,
            &candidate.tool_sequence,
            candidate.occurrence_count,
            candidate.confidence,
            &proposals_dir,
            &prefs.skills.skills_dir,
        ) {
            Ok(path) => {
                let _ = skills::set_skill_draft_path(store, &candidate.name, &path);
                result.skill_drafts_written += 1;
            }
            Err(e) => {
                eprintln!("[consolidator] warn: could not draft skill {}: {e}", candidate.name);
            }
        }
    }

    // ── Stage 4: Gap-driven proposals ─────────────────────────────────────────
    let gap_proposals = skills::detect_gap_proposals(store, 3)?;
    result.gap_proposals = gap_proposals.len();

    for (i, gap) in gap_proposals.iter().enumerate() {
        let proposal = json!({
            "proposal_type": "pref_note",
            "tool_name":  gap.tool_name,
            "query_text": gap.query_text,
            "seen_count": gap.seen_count,
            "proposed_note": gap.proposed_note,
        });
        let path = proposals_dir.join(format!("pref_gap_{i:02}.json"));
        std::fs::write(path, serde_json::to_string_pretty(&proposal)?)?;

        // Record in proposals table with content hash for dedup.
        let content_hash = {
            use std::fmt::Write;
            let mut h = String::new();
            write!(h, "gap:{}:{}", gap.tool_name, gap.query_text).ok();
            format!("{:x}", simple_hash(h.as_bytes()))
        };

        let _ = store.conn().execute(
            "INSERT OR IGNORE INTO proposals
                 (proposal_type, content_hash, target_file, proposed_text, evidence, status)
             VALUES ('pref_note', ?1, '.cortex/prefs.toml', ?2, ?3, 'pending')",
            rusqlite::params![
                content_hash,
                gap.proposed_note,
                json!({"source": "query_gap_log", "seen_count": gap.seen_count}).to_string(),
            ],
        );
    }

    // ── Stage 5: Survival-based proposals (dying patterns → anti-pattern candidates) ─
    result.survival_proposals = propose_survival(store, &proposals_dir)?;

    // ── Stage 6: Instruction proposals (stub — implemented in Phase 2) ────────
    // Phase 2 will add deterministic rule-based instruction edits here.

    // ── Update last-run timestamp ─────────────────────────────────────────────
    let _ = store.conn().execute(
        "INSERT INTO annotations (topic, body, tags, added_at)
         VALUES ('consolidation-last-run', ?1, '[]', ?2)
         ON CONFLICT DO NOTHING",
        rusqlite::params![
            Utc::now().to_rfc3339(),
            Utc::now().to_rfc3339(),
        ],
    );
    // Use REPLACE semantics via a dedicated annotation update.
    let _ = store.conn().execute(
        "UPDATE annotations SET body = ?1, added_at = ?2
         WHERE topic = 'consolidation-last-run'",
        rusqlite::params![Utc::now().to_rfc3339(), Utc::now().to_rfc3339()],
    );
    result.last_run_updated = true;

    Ok(result)
}

// ── Health report ─────────────────────────────────────────────────────────────

fn build_health_report(store: &Store) -> Result<Value> {
    let patterns: i64 = store.conn().query_row(
        "SELECT COUNT(*) FROM patterns", [], |r| r.get(0),
    ).unwrap_or(0);

    let low_survival: i64 = store.conn().query_row(
        "SELECT COUNT(*) FROM patterns WHERE survival_rate < 0.4", [], |r| r.get(0),
    ).unwrap_or(0);

    let anti_patterns: i64 = store.conn().query_row(
        "SELECT COUNT(*) FROM anti_patterns", [], |r| r.get(0),
    ).unwrap_or(0);

    let pending_obs: i64 = store.conn().query_row(
        "SELECT COUNT(*) FROM pending_observations", [], |r| r.get(0),
    ).unwrap_or(0);

    let pending_proposals: i64 = store.conn().query_row(
        "SELECT COUNT(*) FROM proposals WHERE status = 'pending'", [], |r| r.get(0),
    ).unwrap_or(0);

    let orphaned_sessions: i64 = store.conn().query_row(
        "SELECT COUNT(*) FROM protocol_sessions WHERE closeout_run = 0
         AND started_at < (unixepoch() - 7200)",
        [], |r| r.get(0),
    ).unwrap_or(0);

    let hot_gaps: Vec<Value> = {
        let cutoff = Utc::now().timestamp() - 7 * 86400;
        let mut stmt = store.conn().prepare(
            "SELECT query_text, seen_count FROM query_gap_log
             WHERE last_seen_at >= ?1 ORDER BY seen_count DESC LIMIT 5"
        ).unwrap();
        stmt.query_map(rusqlite::params![cutoff], |r| {
            Ok(json!({"query": r.get::<_,String>(0)?, "count": r.get::<_,i64>(1)?}))
        }).unwrap()
        .filter_map(|r| r.ok())
        .collect()
    };

    Ok(json!({
        "generated_at":     Utc::now().to_rfc3339(),
        "patterns":         patterns,
        "low_survival":     low_survival,
        "anti_patterns":    anti_patterns,
        "pending_obs":      pending_obs,
        "pending_proposals": pending_proposals,
        "orphaned_sessions": orphaned_sessions,
        "hot_gaps":         hot_gaps,
    }))
}

// ── Survival proposals ────────────────────────────────────────────────────────

fn propose_survival(store: &Store, proposals_dir: &Path) -> Result<usize> {
    // Find patterns used 3+ times with survival_rate < 0.4.
    let mut stmt = store.conn().prepare(
        "SELECT id, name, intent, body, use_count, reverted_count, survival_rate
         FROM patterns
         WHERE use_count >= 3 AND survival_rate < 0.4
         ORDER BY survival_rate ASC
         LIMIT 10"
    )?;

    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, i64>(4)?,
            row.get::<_, i64>(5)?,
            row.get::<_, f32>(6)?,
        ))
    })?;

    let mut count = 0usize;
    for row in rows {
        let (id, name, intent, body, uses, reverts, rate) = row?;

        let proposal = json!({
            "proposal_type": "dying_pattern",
            "pattern_id":    id,
            "name":          name,
            "intent":        intent,
            "body_preview":  body.chars().take(200).collect::<String>(),
            "use_count":     uses,
            "reverted_count": reverts,
            "survival_rate": rate,
            "suggested_action": "Consider converting to anti-pattern or removing if superseded.",
        });

        let safe_name = name.replace(['/', '\\', ' '], "-");
        let path = proposals_dir.join(format!("ap_dying_{safe_name}.json"));
        std::fs::write(path, serde_json::to_string_pretty(&proposal)?)?;

        // Record in proposals table.
        let content_hash = format!("{:x}", simple_hash(format!("dying:{id}:{name}").as_bytes()));
        let _ = store.conn().execute(
            "INSERT OR IGNORE INTO proposals
                 (proposal_type, content_hash, target_file, proposed_text, evidence, status)
             VALUES ('anti_pattern', ?1, 'patterns', ?2, ?3, 'pending')",
            rusqlite::params![
                content_hash,
                format!("Pattern '{name}' has {rate:.0}% survival after {uses} uses — review for removal or anti-pattern conversion"),
                json!({"pattern_id": id, "use_count": uses, "reverted_count": reverts}).to_string(),
            ],
        );

        count += 1;
    }

    Ok(count)
}

// ── Staleness check ───────────────────────────────────────────────────────────

/// Returns true if the last consolidation run was more than `staleness_hours` ago.
pub fn is_stale(store: &Store, staleness_hours: u32) -> bool {
    let cutoff = Utc::now().timestamp() - (staleness_hours as i64 * 3600);

    let last_run: Option<String> = store.conn().query_row(
        "SELECT body FROM annotations WHERE topic = 'consolidation-last-run' LIMIT 1",
        [], |r| r.get(0),
    ).ok().flatten();

    match last_run {
        None => true, // never run
        Some(ts) => {
            let last_ts = chrono::DateTime::parse_from_rfc3339(&ts)
                .map(|dt| dt.timestamp())
                .unwrap_or(0);
            last_ts < cutoff
        }
    }
}

// ── Simple hash for content dedup ─────────────────────────────────────────────

fn simple_hash(data: &[u8]) -> u64 {
    // FNV-1a 64-bit hash — no external deps needed.
    let mut h: u64 = 14695981039346656037u64;
    for &b in data {
        h ^= b as u64;
        h = h.wrapping_mul(1099511628211u64);
    }
    h
}

// ── Survival proposals (public entry point for CLI) ───────────────────────────

/// Public wrapper called by the CLI `propose-survival` command.
pub fn propose_survival_pub(store: &Store, proposals_dir: &Path) -> Result<usize> {
    propose_survival(store, proposals_dir)
}

#[derive(Debug, Clone)]
pub struct ProposalRow {
    pub id:            i64,
    pub proposal_type: String,
    pub proposed_text: String,
    pub evidence:      String,
    pub created_at:    i64,
}

/// Load all pending proposals from the DB.
pub fn load_pending_proposals(store: &Store) -> Result<Vec<ProposalRow>> {
    let mut stmt = store.conn().prepare(
        "SELECT id, proposal_type, proposed_text, evidence, created_at
         FROM proposals WHERE status = 'pending'
         ORDER BY created_at ASC"
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(ProposalRow {
            id:            row.get(0)?,
            proposal_type: row.get(1)?,
            proposed_text: row.get(2)?,
            evidence:      row.get(3)?,
            created_at:    row.get(4)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// Set a proposal status (pending / approved / rejected).
pub fn set_proposal_status(store: &Store, id: i64, status: &str) -> Result<()> {
    let now = Utc::now().timestamp();
    let (reviewed_col, committed_col) = match status {
        "approved" => (Some(now), Some(now)),
        "rejected" => (Some(now), None),
        _          => (None, None),
    };

    match (reviewed_col, committed_col) {
        (Some(r), Some(c)) => {
            store.conn().execute(
                "UPDATE proposals SET status=?1, reviewed_at=?2, committed_at=?3 WHERE id=?4",
                rusqlite::params![status, r, c, id],
            )?;
        }
        (Some(r), None) => {
            store.conn().execute(
                "UPDATE proposals SET status=?1, reviewed_at=?2 WHERE id=?3",
                rusqlite::params![status, r, id],
            )?;
        }
        _ => {
            store.conn().execute(
                "UPDATE proposals SET status=?1 WHERE id=?2",
                rusqlite::params![status, id],
            )?;
        }
    }
    Ok(())
}

/// Format the pending proposals as a human-readable review list.
pub fn format_pending_proposals(proposals: &[ProposalRow]) -> String {
    if proposals.is_empty() {
        return "No pending cross-session proposals. Run consolidation first.\n".to_string();
    }
    let mut out = format!("{} pending proposal(s):\n\n", proposals.len());
    for p in proposals {
        let age_hours = (Utc::now().timestamp() - p.created_at) / 3600;
        out.push_str(&format!(
            "  [{}] type:{} ({} hours ago)\n  {}\n\n",
            p.id, p.proposal_type, age_hours,
            p.proposed_text.chars().take(120).collect::<String>(),
        ));
    }
    out
}
