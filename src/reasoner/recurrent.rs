//! Recurrent loop — Mythos-inspired iterative refinement.
//! Propose → Critique → Refine → Assess → Halt or Continue

use super::scratchpad::Scratchpad;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecurrentContext {
    pub loop_index: u8,
    pub confidence: f32,
    pub critiques: Vec<String>,
    pub should_continue: bool,
    pub halt_reason: Option<String>,
    pub next_prompt: String,
}

/// Run one iteration of the recurrent loop:
/// 1. Critique the current hypothesis against anti-patterns + graph conflicts
/// 2. Score confidence
/// 3. Check halting conditions
/// 4. Generate next prompt or halt signal
pub fn run_recurrent_loop(
    scratchpad: &mut Scratchpad,
    conn: &Connection,
    loop_index: u8,
    max_loops: u8,
) -> crate::Result<RecurrentContext> {
    let hypothesis = scratchpad.hypotheses.last()
        .ok_or_else(|| anyhow::anyhow!("No hypothesis to critique"))?
        .content.clone();

    // Step 1: Critique hypothesis
    let critiques = critique_hypothesis(&hypothesis, conn)?;
    
    for c in &critiques {
        scratchpad.add_critique(c)?;
    }

    // Step 2: Score confidence
    let confidence = score_confidence(&critiques, conn)?;
    scratchpad.set_confidence(confidence);

    // Step 3: Check halting conditions
    let (should_halt, halt_reason) = should_halt(scratchpad, max_loops);

    // Step 4: Generate response
    let should_continue = !should_halt;
    let halt_reason_str = halt_reason.clone();
    
    if should_halt {
        scratchpad.set_halted(&halt_reason);
    }

    let next_prompt = if should_halt {
        format!("HALT: {}. Final hypothesis ready.", halt_reason)
    } else {
        generate_refine_prompt(scratchpad, loop_index + 1, &critiques)
    };

    Ok(RecurrentContext {
        loop_index,
        confidence,
        critiques,
        should_continue,
        halt_reason: if should_halt { Some(halt_reason_str) } else { None },
        next_prompt,
    })
}

/// Critique hypothesis against anti-patterns and graph conflicts.
fn critique_hypothesis(hypothesis: &str, conn: &Connection) -> crate::Result<Vec<String>> {
    let mut critiques = vec![];

    // Load anti-patterns
    let mut stmt = conn.prepare(
        "SELECT wrong, correct FROM anti_patterns LIMIT 50"
    )?;
    
    let anti_patterns = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
        ))
    })?;

    for ap_result in anti_patterns {
        let (wrong, right) = ap_result?;
        if hypothesis.contains(&wrong) {
            critiques.push(format!(
                "Anti-pattern detected: uses '{}'. Should use '{}' instead.",
                wrong, right
            ));
        }
    }

    // Check graph conflicts (if any nodes are referenced)
    // Simple heuristic: look for type names in hypothesis that might conflict
    let mut stmt = conn.prepare(
        "SELECT ge1.from_id, ge2.to_id
         FROM graph_edges ge1
         JOIN graph_edges ge2 ON ge1.to_id = ge2.from_id
         WHERE ge1.relation = ? AND ge2.relation = ?
         LIMIT 10"
    )?;

    let conflicts = stmt.query_map(
        rusqlite::params!["pairs", "conflicts"],
        |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
    )?;

    for conf_result in conflicts {
        let (from_id, to_id) = conf_result?;
        if hypothesis.contains(&from_id) && hypothesis.contains(&to_id) {
            critiques.push(format!(
                "Graph conflict: '{}' and '{}' marked as conflicting. Reconsider usage.",
                from_id, to_id
            ));
        }
    }

    Ok(critiques)
}

/// Score confidence: ratio of violations / total checks.
/// Higher confidence = fewer violations found.
fn score_confidence(
    critiques: &[String],
    conn: &Connection,
) -> crate::Result<f32> {
    // Count total checks from anti-patterns + graph conflict rules.
    let mut stmt = conn.prepare("SELECT COUNT(*) FROM anti_patterns")?;
    let ap_checks: i64 = stmt.query_row([], |row| row.get(0))?;

    let mut stmt = conn.prepare("SELECT COUNT(*) FROM graph_edges WHERE relation = 'conflicts'")?;
    let conflict_checks: i64 = stmt.query_row([], |row| row.get(0))?;

    let total_checks = ap_checks + conflict_checks;

    if total_checks == 0 {
        return Ok(0.8); // No checks defined → moderate confidence
    }

    let violation_count = critiques.len() as f32;
    let violation_rate = violation_count / total_checks as f32;
    
    let confidence = (1.0 - violation_rate).max(0.0).min(1.0);
    Ok(confidence)
}

/// Determine if we should halt the loop.
fn should_halt(scratchpad: &Scratchpad, max_loops: u8) -> (bool, String) {
    // Halt if confidence threshold reached
    if scratchpad.confidence >= 0.92 {
        return (true, "confidence threshold reached (≥0.92)".to_string());
    }

    // Halt if max loops reached
    if scratchpad.loop_index >= max_loops {
        return (true, format!("max loops ({}) reached", max_loops));
    }

    // Halt if hypothesis is stable
    if scratchpad.is_stable() {
        return (true, "hypothesis stable — no further refinement needed".to_string());
    }

    (false, String::new())
}

/// Generate the prompt for the next refinement loop.
fn generate_refine_prompt(
    scratchpad: &Scratchpad,
    next_loop: u8,
    critiques: &[String],
) -> String {
    let critique_summary = if critiques.is_empty() {
        "No critiques found. Refine for completeness.".to_string()
    } else {
        let top = critiques.iter().take(3).map(|c| format!("  • {}", c)).collect::<Vec<_>>();
        format!("Address these issues:\n{}", top.join("\n"))
    };

    format!(
        "Refine hypothesis (loop {}): Current confidence: {:.0}%\n{}\n\nGenerate improved hypothesis:",
        next_loop,
        scratchpad.confidence * 100.0,
        critique_summary
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_stability_detection() {
        let mut scratchpad = Scratchpad::new("test task");
        scratchpad.add_hypothesis(1, "spawn player at x=0, y=0").ok();
        scratchpad.add_hypothesis(2, "spawn player at x=0, y=0").ok();
        assert!(scratchpad.is_stable(), "Identical hypotheses should be stable");
    }

    #[test]
    fn test_confidence_bounds() {
        let confidence = (1.0_f32 - 0.5_f32).max(0.0_f32).min(1.0_f32);
        assert_eq!(confidence, 0.5);
        assert!((0.0..=1.0).contains(&confidence));
    }
}
