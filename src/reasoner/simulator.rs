//! Simulator — dry-run impact predictor.
//! Predicts what breaks before touching a core type.

use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use crate::graph;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum RiskLevel {
    Low,
    Medium,
    High,
}

impl RiskLevel {
    pub fn as_str(&self) -> &'static str {
        match self {
            RiskLevel::Low => "Low",
            RiskLevel::Medium => "Medium",
            RiskLevel::High => "High",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AffectedItem {
    pub name: String,
    pub relation: String,
    pub impact: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SimulationResult {
    pub changed_item: String,
    pub affected: Vec<AffectedItem>,
    pub risk_level: RiskLevel,
    pub warnings: Vec<String>,
}

impl SimulationResult {
    pub fn render(&self) -> String {
        let risk_emoji = match self.risk_level {
            RiskLevel::Low => "✓",
            RiskLevel::Medium => "⚠",
            RiskLevel::High => "🚨",
        };

        let mut output = format!(
            "{} SIMULATION RESULT for: {}\n\
             Risk Level: {} ({})\n\n",
            risk_emoji, self.changed_item, self.risk_level.as_str(), 
            self.affected.len()
        );

        if self.affected.is_empty() {
            output.push_str("No direct impact detected.\n");
        } else {
            output.push_str("Affected Items:\n");
            for item in &self.affected {
                output.push_str(&format!(
                    "  • {} (via {}): {}\n",
                    item.name, item.relation, item.impact
                ));
            }
        }

        if !self.warnings.is_empty() {
            output.push_str("\nWarnings:\n");
            for warning in &self.warnings {
                output.push_str(&format!("  ⚠ {}\n", warning));
            }
        }

        output
    }
}

/// Simulate the impact of changing an item.
/// Looks up reverse edges (used_by) and classifies impact by relation type.
pub fn simulate_change(
    conn: &Connection,
    item_name: &str,
    change_description: &str,
) -> crate::Result<SimulationResult> {
    let mut affected = vec![];
    let mut warnings = vec![];
    let mut seen: HashSet<(String, String)> = HashSet::new();

    // Find the node ID
    let mut stmt = conn.prepare(
        "SELECT id FROM graph_nodes WHERE name = ?1 LIMIT 1"
    )?;

    let node_id: Option<String> = stmt.query_row(
        [item_name],
        |row| row.get(0),
    ).ok();

    if node_id.is_none() {
        return Ok(SimulationResult {
            changed_item: item_name.to_string(),
            affected: vec![],
            risk_level: RiskLevel::Low,
            warnings: vec!["Item not found in graph — no impact detected.".to_string()],
        });
    }

    let node_id = node_id.unwrap();

    // Use graph reverse lookup for primary impact set (depth 1).
    let users = graph::used_by(conn, &node_id)?;
    for user in users {
        if !seen.insert((user.name.clone(), "uses".to_string())) {
            continue;
        }

        let impact = if change_description.contains("new field") || change_description.contains("signature") {
            format!("'{}' uses this type in fields or return type — construction sites affected", user.name)
        } else {
            format!("'{}' references this type — may need re-evaluation", user.name)
        };

        affected.push(AffectedItem {
            name: user.name,
            relation: "uses".to_string(),
            impact,
        });
    }

    // Include any non-uses reverse relations for additional warnings.
    let mut stmt = conn.prepare(
        "SELECT ge.from_id, ge.relation, gn.name
         FROM graph_edges ge
         JOIN graph_nodes gn ON ge.from_id = gn.id
         WHERE ge.to_id = ?1
         ORDER BY ge.weight DESC
         LIMIT 20"
    )?;

    let uses = stmt.query_map([&node_id], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
        ))
    })?;

    for use_result in uses {
        let (_from_id, relation, from_name) = use_result?;
        let relation_norm = relation.to_lowercase();

        if !seen.insert((from_name.clone(), relation_norm.clone())) {
            continue;
        }
        
        let impact = match relation_norm.as_str() {
            "implements" => {
                warnings.push(format!("Trait implementation: '{}' implements this trait. Contract may need updating.", from_name));
                "trait contract may need updating".to_string()
            },
            "uses" => {
                if change_description.contains("new field") || change_description.contains("signature") {
                    format!("'{}' uses this type in fields or return type — construction sites affected", from_name)
                } else {
                    format!("'{}' references this type — may need re-evaluation", from_name)
                }
            },
            "pairs" => {
                warnings.push(format!("Paired usage detected: '{}' is marked to be used with this item.", from_name));
                "paired usage — may require coordinated update".to_string()
            },
            "conflicts" => {
                warnings.push(format!("Conflict detected: '{}' is marked as conflicting with this item.", from_name));
                "conflicts flagged — reconsider change or add guard logic".to_string()
            },
            "derived_from" => {
                "derived from/variant — parent change may cascade".to_string()
            },
            "calls" => {
                "function call site — callers may break".to_string()
            },
            _ => "unknown relation — requires manual review".to_string(),
        };

        affected.push(AffectedItem {
            name: from_name,
            relation: relation.clone(),
            impact,
        });
    }

    // Classify risk
    let risk_level = match affected.len() {
        0..=2 => RiskLevel::Low,
        3..=6 => RiskLevel::Medium,
        _ => RiskLevel::High,
    };

    if risk_level == RiskLevel::High {
        warnings.insert(0, "HIGH RISK: Many items depend on this. Plan for wide-ranging re-testing.".to_string());
    }

    Ok(SimulationResult {
        changed_item: item_name.to_string(),
        affected,
        risk_level,
        warnings,
    })
}

/// Extended simulation with depth-2 transitive lookup.
pub fn simulate_change_deep(
    conn: &Connection,
    item_name: &str,
    change_description: &str,
    depth: u8,
) -> crate::Result<SimulationResult> {
    let mut result = simulate_change(conn, item_name, change_description)?;

    if depth > 1 && !result.affected.is_empty() {
        // Transitive: check what uses the users
        let mut transitive_affected = vec![];
        
        for item in &result.affected {
            if let Ok(transitive_result) = simulate_change(conn, &item.name, "transitive check") {
                transitive_affected.extend(transitive_result.affected);
            }
        }

        if !transitive_affected.is_empty() {
            result.warnings.push(format!(
                "Transitive impact: {} more items affected indirectly",
                transitive_affected.len()
            ));
            result.risk_level = RiskLevel::High;
        }
    }

    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_risk_classification() {
        assert_eq!(
            match 1 { 0..=2 => RiskLevel::Low, _ => RiskLevel::High },
            RiskLevel::Low
        );
        assert_eq!(
            match 5 { 0..=2 => RiskLevel::Low, 3..=6 => RiskLevel::Medium, _ => RiskLevel::High },
            RiskLevel::Medium
        );
        assert_eq!(
            match 10 { 0..=2 => RiskLevel::Low, 3..=6 => RiskLevel::Medium, _ => RiskLevel::High },
            RiskLevel::High
        );
    }

    #[test]
    fn test_render_output() {
        let result = SimulationResult {
            changed_item: "Action".to_string(),
            affected: vec![
                AffectedItem {
                    name: "GameObject".to_string(),
                    relation: "Uses".to_string(),
                    impact: "field type change".to_string(),
                },
            ],
            risk_level: RiskLevel::Medium,
            warnings: vec!["Consider backwards compat.".to_string()],
        };
        let rendered = result.render();
        assert!(rendered.contains("Action"));
        assert!(rendered.contains("Medium"));
        assert!(rendered.contains("GameObject"));
    }
}
