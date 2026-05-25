use anyhow::Result;
use rusqlite::params;

use crate::compressor::{build_term_vector_str, cosine_similarity};
use crate::memory::Store;

/// Find pairs of patterns whose content is highly similar (potential duplicates).
///
/// Returns `(kept_id, merged_id, score, kept_name, merged_name)` sorted by score desc.
pub fn find_candidates(
    store: &Store,
    threshold: f32,
) -> Result<Vec<(i64, i64, f32, String, String)>> {
    let patterns = store.all_patterns()?;
    let mut candidates = Vec::new();

    for i in 0..patterns.len() {
        // Skip dead patterns (survival_rate near 0).
        if patterns[i].survival_rate < 0.1 {
            continue;
        }
        let text_i = format!(
            "{} {} {}",
            patterns[i].intent,
            patterns[i].body,
            patterns[i].uses.join(" ")
        );
        let tv_i = build_term_vector_str(&text_i);

        for j in (i + 1)..patterns.len() {
            if patterns[j].survival_rate < 0.1 {
                continue;
            }
            let text_j = format!(
                "{} {} {}",
                patterns[j].intent,
                patterns[j].body,
                patterns[j].uses.join(" ")
            );
            let tv_j = build_term_vector_str(&text_j);

            let score = cosine_similarity(&tv_i, &tv_j);
            if score >= threshold {
                if let (Some(id_i), Some(id_j)) = (patterns[i].id, patterns[j].id) {
                    candidates.push((
                        id_i,
                        id_j,
                        score,
                        patterns[i].name.clone(),
                        patterns[j].name.clone(),
                    ));
                }
            }
        }
    }

    candidates.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal));
    Ok(candidates)
}

/// Soft-delete the merged pattern (mark reverted to floor survival) and record the merge log.
pub fn merge_patterns(store: &Store, keep_id: i64, discard_id: i64, score: f32) -> Result<()> {
    store.conn().execute(
        "UPDATE patterns SET reverted_count = 999, survival_rate = 0.0 WHERE id = ?1",
        params![discard_id],
    )?;
    store.insert_merge_log(
        keep_id,
        discard_id,
        score,
        "consolidated via cosine similarity",
    )?;
    Ok(())
}
