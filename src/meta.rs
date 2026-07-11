/// Phase 5b: Meta-analysis for the consolidation pipeline.
///
/// Analyzes pipeline outputs to track proposal effectiveness and detect
/// threshold drift. All findings are staged as `meta_*` proposals in the
/// existing proposals table — never auto-applied, never source-modifying.
use std::path::Path;

use anyhow::Result;
use chrono::Utc;
use serde::Serialize;
use serde_json::{json, Value};

use crate::memory::Store;
use crate::verify;

// ── Meta report ───────────────────────────────────────────────────────────────

#[derive(Debug, Default, Serialize)]
pub struct MetaReport {
    pub generated_at:        String,
    pub total_proposals:     usize,
    pub approved:            usize,
    pub rejected:            usize,
    pub pending:             usize,
    pub trial:               usize,
    pub approval_rate:       f32,
    pub gate_rejection_rate: f32,
    pub top_rejected_types:  Vec<(String, usize)>,
}

/// Build a meta-analysis report from the proposals table + rejection log.
pub fn build_meta_report(store: &Store, rejected_log: &Path) -> Result<MetaReport> {
    let total: i64 = store.conn().query_row(
        "SELECT COUNT(*) FROM proposals", [], |r| r.get(0),
    ).unwrap_or(0);

    let approved: i64 = store.conn().query_row(
        "SELECT COUNT(*) FROM proposals WHERE status = 'approved'", [], |r| r.get(0),
    ).unwrap_or(0);

    let rejected: i64 = store.conn().query_row(
        "SELECT COUNT(*) FROM proposals WHERE status = 'rejected'", [], |r| r.get(0),
    ).unwrap_or(0);

    let pending: i64 = store.conn().query_row(
        "SELECT COUNT(*) FROM proposals WHERE status = 'pending'", [], |r| r.get(0),
    ).unwrap_or(0);

    let trial: i64 = store.conn().query_row(
        "SELECT COUNT(*) FROM proposals WHERE status = 'trial'", [], |r| r.get(0),
    ).unwrap_or(0);

    let approval_rate = if total > 0 {
        approved as f32 / total as f32
    } else {
        0.0
    };

    // Count gate rejection types from proposals table.
    let mut rejected_by_type: Vec<(String, usize)> = {
        let mut stmt = store.conn().prepare(
            "SELECT proposal_type, COUNT(*) as cnt FROM proposals
             WHERE status = 'rejected'
             GROUP BY proposal_type ORDER BY cnt DESC LIMIT 5"
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, usize>(1)?))
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    };

    // Also scan rejection log for gate-level rejection counts.
    if let Ok(content) = std::fs::read_to_string(rejected_log) {
        let mut gate_counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
        for line in content.lines() {
            if let Ok(v) = serde_json::from_str::<Value>(line) {
                if let Some(reason) = v.get("reason").and_then(|r| r.as_str()) {
                    let gate = if reason.starts_with("duplicate:") { "duplicate"
                        } else if reason.starts_with("rust_syntax:") { "rust_syntax"
                        } else if reason.starts_with("credibility:") { "credibility"
                        } else if reason.starts_with("gap_trial:") { "gap_trial"
                        } else if reason.starts_with("survival_trend:") { "survival_trend"
                        } else { "other" };
                    *gate_counts.entry(gate.to_string()).or_default() += 1;
                }
            }
        }
        // Merge gate counts into rejected_by_type as "gate:<name>" entries.
        for (gate, count) in gate_counts {
            rejected_by_type.push((format!("gate:{gate}"), count));
        }
    }
    rejected_by_type.sort_by(|a, b| b.1.cmp(&a.1));

    let trej = rejected as f64;
    let tpro = total as f64;
    let sum_non_gate: usize = rejected_by_type.iter()
        .filter(|(t, _)| !t.starts_with("gate:"))
        .map(|(_, c)| c)
        .sum();
    let gate_rejection_rate = if tpro > 0.0 {
        ((trej - sum_non_gate as f64).max(0.0) / tpro) as f32
    } else {
        0.0
    };

    Ok(MetaReport {
        generated_at: Utc::now().to_rfc3339(),
        total_proposals: total as usize,
        approved: approved as usize,
        rejected: rejected as usize,
        pending: pending as usize,
        trial: trial as usize,
        approval_rate,
        gate_rejection_rate,
        top_rejected_types: rejected_by_type,
    })
}

/// Stage a meta-proposal if analysis reveals a configurable issue.
/// Example: if credibility gate rejects >60% of proposals, suggest raising min_cred.
pub fn stage_meta_proposals(store: &Store, report: &MetaReport) -> Result<usize> {
    let mut staged = 0usize;

    let _content_hash_base = format!("meta_{}", Utc::now().timestamp());
    let simple_hash = |data: &[u8]| -> String {
        let mut h: u64 = 14695981039346656037u64;
        for &b in data {
            h ^= b as u64;
            h = h.wrapping_mul(1099511628211u64);
        }
        format!("{:x}", h)
    };

    // Rule 1: If gate rejection rate > 60%, suggest threshold review.
    if report.gate_rejection_rate > 0.6 && report.total_proposals >= 5 {
        let hash = simple_hash(b"meta:gate_rejection_rate");
        let rejected_log = Path::new(".cortex").join("rejected-proposals.jsonl");
        let rejected_log = if rejected_log.exists() { rejected_log } else { Path::new(".").join("rejected-proposals.jsonl") };
        if !verify::is_recently_rejected(&rejected_log, &hash) {
            let proposed_text = format!(
                "Meta: Gate rejection rate is {:.0}% across {} proposals. \
                 Review verification thresholds (credibility filter, gap trial).",
                report.gate_rejection_rate * 100.0, report.total_proposals
            );
            let evidence = json!({
                "source": "meta_analysis",
                "gate_rejection_rate": report.gate_rejection_rate,
                "approval_rate": report.approval_rate,
                "total_proposals": report.total_proposals,
            });
            let _ = store.conn().execute(
                "INSERT OR IGNORE INTO proposals
                 (proposal_type, content_hash, target_file, proposed_text, evidence, status, gate_signals)
                 VALUES ('meta_threshold', ?1, '.cortex/prefs.toml', ?2, ?3, 'pending', '{\"gate\":\"meta_analysis\"}')",
                rusqlite::params![hash, proposed_text, evidence.to_string()],
            );
            staged += 1;
        }
    }

    // Rule 2: If approval rate is 0 after 5+ proposals, flag for investigation.
    if report.approval_rate == 0.0 && report.total_proposals >= 5 {
        let hash = simple_hash(b"meta:zero_approval");
        let proposed_text = format!(
            "Meta: Zero proposals approved out of {} total. \
             Pipeline may be generating low-quality proposals — review gate thresholds.",
            report.total_proposals
        );
        let evidence = json!({
            "source": "meta_analysis",
            "total_proposals": report.total_proposals,
            "approved": 0,
        });
        let _ = store.conn().execute(
            "INSERT OR IGNORE INTO proposals
             (proposal_type, content_hash, target_file, proposed_text, evidence, status, gate_signals)
             VALUES ('meta_threshold', ?1, '.cortex/prefs.toml', ?2, ?3, 'pending', '{\"gate\":\"meta_analysis\"}')",
            rusqlite::params![hash, proposed_text, evidence.to_string()],
        );
        staged += 1;
    }

    // Rule 3: If any single rejection type dominates (>70%), suggest per-type threshold.
    for (rtype, count) in &report.top_rejected_types {
        if report.total_proposals >= 5 && *count as f32 / report.total_proposals as f32 > 0.7 {
            let hash = simple_hash(format!("meta:dominant_rejection:{rtype}").as_bytes());
            let proposed_text = format!(
                "Meta: '{rtype}' accounts for {}/{} rejections ({:.0}%). \
                 Consider adjusting this gate threshold.",
                count, report.total_proposals, (*count as f32 / report.total_proposals as f32) * 100.0
            );
            let evidence = json!({
                "source": "meta_analysis",
                "dominant_type": rtype,
                "count": count,
                "total": report.total_proposals,
            });
            let _ = store.conn().execute(
                "INSERT OR IGNORE INTO proposals
                 (proposal_type, content_hash, target_file, proposed_text, evidence, status, gate_signals)
                 VALUES ('meta_threshold', ?1, '.cortex/prefs.toml', ?2, ?3, 'pending', '{\"gate\":\"meta_analysis\"}')",
                rusqlite::params![hash, proposed_text, evidence.to_string()],
            );
            staged += 1;
        }
    }

    Ok(staged)
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::Store;
    use std::path::PathBuf;

    fn test_store(name: &str) -> Store {
        let dir = std::env::temp_dir().join("cortex-meta-test");
        let _ = std::fs::create_dir_all(&dir);
        let db = dir.join(format!("{name}.db"));
        let _ = std::fs::remove_file(&db);
        Store::open(&db).unwrap()
    }

    #[test]
    fn meta_report_empty_db() {
        let store = test_store("empty");
        let log = std::env::temp_dir().join("cortex-meta-test-empty.jsonl");
        let report = build_meta_report(&store, &log).unwrap();
        assert_eq!(report.total_proposals, 0);
        assert_eq!(report.approval_rate, 0.0);
    }

    #[test]
    fn meta_report_counts_proposals() {
        let store = test_store("counts");
        let log = std::env::temp_dir().join("cortex-meta-test-counts.jsonl");

        // Insert a few proposals with different statuses.
        store.conn().execute_batch(
            "INSERT INTO proposals (proposal_type, content_hash, target_file, proposed_text, evidence, status, gate_signals)
             VALUES ('pref_note', 'a', 'x', 'test', '{}', 'approved', '{}');
             INSERT INTO proposals (proposal_type, content_hash, target_file, proposed_text, evidence, status, gate_signals)
             VALUES ('pref_note', 'b', 'x', 'test', '{}', 'rejected', '{}');
             INSERT INTO proposals (proposal_type, content_hash, target_file, proposed_text, evidence, status, gate_signals)
             VALUES ('pref_note', 'c', 'x', 'test', '{}', 'pending', '{}');
             INSERT INTO proposals (proposal_type, content_hash, target_file, proposed_text, evidence, status, gate_signals)
             VALUES ('pref_note', 'd', 'x', 'test', '{}', 'trial', '{}');"
        ).unwrap();

        let report = build_meta_report(&store, &log).unwrap();
        assert_eq!(report.total_proposals, 4);
        assert_eq!(report.approved, 1);
        assert_eq!(report.rejected, 1);
        assert_eq!(report.pending, 1);
        assert_eq!(report.trial, 1);
    }
}
