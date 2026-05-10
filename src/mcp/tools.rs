use std::path::Path;

use serde_json::{json, Value};

use crate::cache::{render_with_session, sha256_hex, SessionRegistry};
use crate::git;
use crate::graph;
use crate::memory::Store;
use crate::model::{CodeUnit, PendingObservation};
use crate::planner::{build_context_packet, render_packet};
use crate::search::{keyword_search, semantic_search};

pub fn dispatch(
    tool: &str,
    args: &Value,
    store: &Store,
    units: &[CodeUnit],
    sessions: &SessionRegistry,
    session_id: &str,
    repo_root: &Path,
    prefs_summary: &str,
) -> Result<Value, String> {
    let text = match tool {
        "semantic_search"   => tool_semantic_search(args, units, sessions, session_id),
        "get_item"          => tool_get_item(args, units, sessions, session_id),
        "get_context"       => tool_get_context(args, store, units, repo_root, prefs_summary),
        "get_delta"         => tool_get_delta(args, repo_root),
        "query_graph"       => tool_query_graph(args, store),
        "get_preferences"   => tool_get_preferences(prefs_summary),
        "recurrent_think"   => tool_recurrent_think(args, store),
        "simulate_change"   => tool_simulate_change(args, store),
        "recall"            => tool_recall(args, store, units, sessions, session_id),
        "list_patterns"     => tool_list_patterns(store),
        "get_anti_patterns" => tool_get_anti_patterns(store),
        "suggest_pattern"   => tool_suggest_pattern(args, store),
        "list_all"          => tool_list_all(args, units),
        other               => Err(format!("unknown tool: {other}")),
    }?;

    Ok(json!({ "content": [{ "type": "text", "text": text }] }))
}

// ── semantic_search ───────────────────────────────────────────────────────────

fn tool_semantic_search(
    args: &Value,
    units: &[CodeUnit],
    sessions: &SessionRegistry,
    session_id: &str,
) -> Result<String, String> {
    let query = args["query"].as_str().ok_or("missing `query`")?;
    let limit = args["limit"].as_u64().unwrap_or(5) as usize;

    let results = semantic_search(query, units, limit);
    let keyword = keyword_search(query, units);

    if results.is_empty() && keyword.is_empty() {
        return Ok(format!("No results for `{query}`."));
    }

    let mut header = format!("Search: `{query}`\n\n");

    // Build (hash, text) pairs for session-aware rendering
    let mut items: Vec<(String, String)> = Vec::new();

    if !results.is_empty() {
        header.push_str("## Semantic matches\n");
        for r in &results {
            let entry = format!(
                "### `{}` ({:.0}% match)\n{}\n",
                r.unit.name, r.score * 100.0, r.unit.compressed
            );
            let hash = sha256_hex(entry.as_bytes());
            items.push((hash, entry));
        }
    }

    // Keyword-only extras not already in semantic results
    let semantic_ids: Vec<&str> = results.iter().map(|r| r.unit.id.as_str()).collect();
    let extras: Vec<_> = keyword.iter()
        .filter(|u| !semantic_ids.contains(&u.id.as_str()))
        .take(3)
        .collect();

    let mut out = header;
    out.push_str(&render_with_session(&items, sessions, session_id));

    if !extras.is_empty() {
        out.push_str("## Keyword matches\n");
        for u in extras {
            out.push_str(&format!("- `{}` ({}): {}\n", u.name, u.kind, u.summary));
        }
    }

    Ok(out)
}

// ── get_item ──────────────────────────────────────────────────────────────────

fn tool_get_item(
    args: &Value,
    units: &[CodeUnit],
    sessions: &SessionRegistry,
    session_id: &str,
) -> Result<String, String> {
    let name = args["name"].as_str().ok_or("missing `name`")?;

    let unit = units.iter().find(|u| u.name == name)
        .ok_or_else(|| format!("no item named `{name}`"))?;

    let header = format!("# `{}` ({})\n\nmodule: `{}`\n\n",
        unit.name, unit.kind, unit.module_path);

    let hash = sha256_hex(unit.compressed.as_bytes());
    let rendered = render_with_session(
        &[(hash, unit.compressed.clone())],
        sessions,
        session_id,
    );

    Ok(format!("{header}{rendered}"))
}

// ── get_context ───────────────────────────────────────────────────────────────

fn tool_get_context(
    args: &Value,
    store: &Store,
    units: &[CodeUnit],
    repo_root: &Path,
    prefs_summary: &str,
) -> Result<String, String> {
    let hint = args["hint"].as_str().ok_or("missing `hint`")?;
    let budget = args["token_budget"].as_u64().unwrap_or(2000) as usize;

    // Augment hint with matching unit summaries for better semantic retrieval
    let augmented = augment_hint(hint, units);

    let delta_opts = crate::git::DeltaOptions {
        include: args.get("delta_include").and_then(|v| v.as_str()).map(str::to_string),
        exclude: args.get("delta_exclude").and_then(|v| v.as_str()).map(str::to_string),
        max_files: args.get("delta_max_files").and_then(|v| v.as_u64()).unwrap_or(8) as usize,
        max_patch_lines: args.get("delta_max_patch_lines").and_then(|v| v.as_u64()).unwrap_or(40) as usize,
    };

    let packet = build_context_packet(store, &augmented, budget, Some(repo_root), Some(&delta_opts))
        .map_err(|e| e.to_string())?;

    if packet.relevant_units.is_empty()
        && packet.patterns.is_empty()
        && packet.anti_patterns.is_empty()
        && packet.annotations.is_empty()
    {
        return Ok(format!(
            "No context found for `{hint}`. Run `cortex index` if the index is empty."
        ));
    }

    let mut out = String::new();
    if !prefs_summary.trim().is_empty() {
        out.push_str(prefs_summary);
        out.push('\n');
    }
    out.push_str(&render_packet(&packet));
    Ok(out)
}

fn tool_get_delta(args: &Value, repo_root: &Path) -> Result<String, String> {
    let opts = crate::git::DeltaOptions {
        include: args.get("include").and_then(|v| v.as_str()).map(str::to_string),
        exclude: args.get("exclude").and_then(|v| v.as_str()).map(str::to_string),
        max_files: args.get("max_files").and_then(|v| v.as_u64()).unwrap_or(128) as usize,
        max_patch_lines: args.get("max_patch_lines").and_then(|v| v.as_u64()).unwrap_or(40) as usize,
    };

    let deltas = if let Some(since) = args.get("since").and_then(|v| v.as_str()) {
        git::commit_deltas_with_options(repo_root, since, "HEAD", &opts).map_err(|e| e.to_string())?
    } else {
        git::head_deltas_with_options(repo_root, &opts).map_err(|e| e.to_string())?
    };

    if deltas.is_empty() {
        return Ok("No git deltas found.".to_string());
    }

    let mut out = String::new();
    for d in deltas {
        let entry = git::compress_delta(&d);
        out.push_str(&format!("{} {} - {}\n", entry.change, entry.path, entry.summary));
    }
    Ok(out)
}

fn tool_query_graph(args: &Value, store: &Store) -> Result<String, String> {
    let name = args["name"].as_str().ok_or("missing `name`")?;
    let depth = args["depth"].as_u64().unwrap_or(1) as u8;

    let unit = store.get_unit(name).map_err(|e| e.to_string())?;
    let Some(root) = unit else {
        return Ok(format!("No graph node found for `{}`", name));
    };

    let (edges, _) = graph::subgraph(store.conn(), &root.id, depth).map_err(|e| e.to_string())?;
    if edges.is_empty() {
        return Ok(format!("{}\n  (no graph neighbors)", root.name));
    }

    let mut out = String::new();
    out.push_str(&format!("{}\n", root.name));
    for e in edges {
        out.push_str(&format!("  -[{}]-> {}\n", e.relation.as_str(), e.to_id));
    }
    Ok(out)
}

fn tool_get_preferences(prefs_summary: &str) -> Result<String, String> {
    if prefs_summary.trim().is_empty() {
        return Ok("No preferences configured.".to_string());
    }
    Ok(prefs_summary.to_string())
}

// ── recall ────────────────────────────────────────────────────────────────────

fn tool_recall(
    args: &Value,
    store: &Store,
    units: &[CodeUnit],
    sessions: &SessionRegistry,
    session_id: &str,
) -> Result<String, String> {
    let topic = args["topic"].as_str().ok_or("missing `topic`")?;
    let topic_lower = topic.to_lowercase();

    let mut out = format!("# Recall: `{topic}`\n\n");
    let mut found = false;

    // API units
    let mut unit_items: Vec<(String, String)> = Vec::new();
    for u in units.iter().filter(|u| {
        u.name.to_lowercase().contains(&topic_lower)
            || u.compressed.to_lowercase().contains(&topic_lower)
    }).take(4) {
        let hash = sha256_hex(u.compressed.as_bytes());
        unit_items.push((hash, u.compressed.clone()));
        found = true;
    }

    if !unit_items.is_empty() {
        out.push_str("## API\n");
        out.push_str(&render_with_session(&unit_items, sessions, session_id));
    }

    // Patterns
    let patterns = store.all_patterns().map_err(|e| e.to_string())?;
    let matched_patterns: Vec<_> = patterns.iter().filter(|p| {
        p.name.to_lowercase().contains(&topic_lower)
            || p.intent.to_lowercase().contains(&topic_lower)
            || p.uses.iter().any(|u| u.to_lowercase().contains(&topic_lower))
            || p.tags.iter().any(|t| t.to_lowercase().contains(&topic_lower))
    }).collect();

    if !matched_patterns.is_empty() {
        found = true;
        out.push_str("## Patterns\n");
        for p in &matched_patterns {
            out.push_str(&format!("### {} — {}\n", p.name, p.intent));
            out.push_str(&p.body);
            out.push('\n');
            if let Some(id) = p.id { let _ = store.pattern_used(id); }
        }
    }

    // Anti-patterns
    let aps = store.all_anti_patterns().map_err(|e| e.to_string())?;
    let matched_aps: Vec<_> = aps.iter().filter(|ap| {
        ap.description.to_lowercase().contains(&topic_lower)
            || ap.wrong.to_lowercase().contains(&topic_lower)
            || ap.tags.iter().any(|t| t.to_lowercase().contains(&topic_lower))
    }).collect();

    if !matched_aps.is_empty() {
        found = true;
        out.push_str("## ⚠ Anti-patterns\n");
        for ap in &matched_aps {
            out.push_str(&format!("✗ {}\n  wrong:   {}\n  correct: {}\n\n",
                ap.description, ap.wrong, ap.correct));
        }
    }

    // Annotations
    let annotations = store.all_annotations().map_err(|e| e.to_string())?;
    let matched_annotations: Vec<_> = annotations.iter().filter(|a| {
        a.topic.to_lowercase().contains(&topic_lower)
            || a.body.to_lowercase().contains(&topic_lower)
            || a.tags.iter().any(|t| t.to_lowercase().contains(&topic_lower))
    }).collect();

    if !matched_annotations.is_empty() {
        found = true;
        out.push_str("## Notes\n");
        for a in &matched_annotations {
            out.push_str(&format!("[{}] {}\n", a.topic, a.body));
        }
    }

    if !found {
        out.push_str("Nothing found. Consider adding an annotation.\n");
    }

    Ok(out)
}

// ── list_patterns ─────────────────────────────────────────────────────────────

fn tool_list_patterns(store: &Store) -> Result<String, String> {
    let patterns = store.all_patterns().map_err(|e| e.to_string())?;
    if patterns.is_empty() {
        return Ok("No approved patterns yet.".into());
    }
    let mut out = format!("{} approved pattern(s):\n\n", patterns.len());
    for p in &patterns {
        let marker = if p.survival_rate < 0.4 {
            "⚠"
        } else if p.survival_rate < 0.8 {
            "!"
        } else {
            "✓"
        };
        out.push_str(&format!(
            "## {} {} (used {}x, reverted {}, survival {:.0}%)\nIntent: {}\n",
            marker,
            p.name,
            p.use_count,
            p.reverted_count,
            p.survival_rate * 100.0,
            p.intent
        ));
        if !p.uses.is_empty() {
            out.push_str(&format!("Uses: {}\n", p.uses.join(", ")));
        }
        out.push_str(&p.body);
        out.push('\n');
    }
    Ok(out)
}

// ── get_anti_patterns ─────────────────────────────────────────────────────────

fn tool_get_anti_patterns(store: &Store) -> Result<String, String> {
    let aps = store.all_anti_patterns().map_err(|e| e.to_string())?;
    if aps.is_empty() {
        return Ok("No anti-patterns recorded yet.".into());
    }
    let mut out = format!("{} anti-pattern(s) — DO NOT do these:\n\n", aps.len());
    for ap in &aps {
        out.push_str(&format!("### {}\n✗ wrong:   {}\n✓ correct: {}\n\n",
            ap.description, ap.wrong, ap.correct));
    }
    Ok(out)
}

// ── suggest_pattern ───────────────────────────────────────────────────────────

fn tool_suggest_pattern(args: &Value, store: &Store) -> Result<String, String> {
    let name   = args["name"].as_str().ok_or("missing `name`")?;
    let intent = args["intent"].as_str().ok_or("missing `intent`")?;
    let body   = args["body"].as_str().ok_or("missing `body`")?;
    let uses: Vec<String> = args["uses"]
        .as_array()
        .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
        .unwrap_or_default();

    let obs = PendingObservation {
        id: None,
        path: format!("pattern/{}", name.to_lowercase().replace(' ', "_")),
        summary: format!("Copilot suggested pattern: `{}` — {}", name, intent),
        diff_hint: format!("name: {name}\nintent: {intent}\nuses: {}\n\n{body}",
            uses.join(", ")),
        observed_at: chrono::Utc::now(),
    };

    let id = store.add_observation(&obs).map_err(|e| e.to_string())?;

    Ok(format!(
        "Pattern suggestion queued (observation id: {}).\n\
         Run `cortex review` then `cortex crystallize {}` to approve.",
        id, id
    ))
}

// ── list_all ──────────────────────────────────────────────────────────────────

fn tool_list_all(args: &Value, units: &[CodeUnit]) -> Result<String, String> {
    let kind_filter = args["kind"].as_str();

    let filtered: Vec<_> = units.iter()
        .filter(|u| kind_filter.map_or(true, |k| u.kind == k))
        .collect();

    if filtered.is_empty() {
        return Ok(match kind_filter {
            Some(k) => format!("No items of kind `{k}`."),
            None    => "Index is empty. Run `cortex index`.".into(),
        });
    }

    let mut by_kind: std::collections::BTreeMap<&str, Vec<&&CodeUnit>> =
        std::collections::BTreeMap::new();
    for u in &filtered {
        by_kind.entry(u.kind.as_str()).or_default().push(u);
    }

    let mut out = format!("{} item(s):\n\n", filtered.len());
    for (kind, items) in &by_kind {
        out.push_str(&format!("## {} ({})\n", kind, items.len()));
        for u in items {
            out.push_str(&format!("- `{}` — {}\n", u.name, u.summary));
        }
        out.push('\n');
    }

    Ok(out)
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn augment_hint(hint: &str, units: &[CodeUnit]) -> String {
    let hint_lower = hint.to_lowercase();
    let extras: Vec<String> = units.iter()
        .filter(|u| hint_lower.contains(&u.name.to_lowercase()))
        .map(|u| u.summary.clone())
        .collect();

    if extras.is_empty() {
        hint.to_string()
    } else {
        format!("{} {}", hint, extras.join(" "))
    }
}

// ── recurrent_think ───────────────────────────────────────────────────────────

fn tool_recurrent_think(args: &Value, store: &Store) -> Result<String, String> {
    let task = args["task"].as_str().ok_or("missing `task`")?;
    let hypothesis = args["hypothesis"].as_str();
    let loop_index = args["loop"].as_u64().unwrap_or(0) as u8;
    let depth_mode = args["depth_mode"].as_str().unwrap_or("auto");
    let max_loops = match depth_mode {
        "shallow" => 2u8,
        "deep" => args["max_loops"].as_u64().unwrap_or(12).min(16) as u8,
        _ => args["max_loops"].as_u64().unwrap_or(6).min(16) as u8,
    };

    // Load persisted scratchpad from SQLite or initialize a new one.
    let mut scratchpad = crate::reasoner::scratchpad::load_from_db(store.conn(), task)
        .map_err(|e| format!("Failed to load scratchpad: {}", e))?
        .unwrap_or_else(|| crate::reasoner::scratchpad::Scratchpad::new(task));

    // Add hypothesis if provided. If this is the first invocation and no hypothesis
    // was provided, seed one from task text so the loop can critique/refine.
    if let Some(h) = hypothesis {
        let next_loop = if loop_index == 0 {
            scratchpad.loop_index.saturating_add(1).max(1)
        } else {
            loop_index
        };
        scratchpad
            .add_hypothesis(next_loop, h)
            .map_err(|e| format!("Failed to add hypothesis: {}", e))?;
    } else if scratchpad.hypotheses.is_empty() {
        scratchpad
            .add_hypothesis(1, &format!("Initial hypothesis for task: {task}"))
            .map_err(|e| format!("Failed to seed hypothesis: {}", e))?;
    }

    if scratchpad.hypotheses.is_empty() {
        return Ok(
            "No hypothesis available yet. Call recurrent_think again with a `hypothesis` argument to begin critique/refine loops."
                .to_string(),
        );
    }

    let active_loop = scratchpad.loop_index.max(1);

    // Run critique + refine cycle
    let context = crate::reasoner::recurrent::run_recurrent_loop(
        &mut scratchpad,
        store.conn(),
        active_loop,
        max_loops,
    ).map_err(|e| format!("Recurrent loop failed: {}", e))?;

    // Persist scratchpad
    crate::reasoner::scratchpad::save_to_db(store.conn(), &scratchpad)
        .map_err(|e| format!("Failed to save scratchpad: {}", e))?;

    // Return context
    let mut output = format!(
        "=== RECURRENT THINKING (Loop {}) ===\n\n\
         Confidence: {:.0}%\n\
         Depth Mode: {}\n\
         Continue: {}\n\n",
        context.loop_index,
        context.confidence * 100.0,
        depth_mode,
        context.should_continue
    );

    if !context.critiques.is_empty() {
        output.push_str("Critiques:\n");
        for c in &context.critiques {
            output.push_str(&format!("  • {}\n", c));
        }
        output.push('\n');
    }

    if let Some(reason) = &context.halt_reason {
        output.push_str(&format!("HALTED: {}\n\n", reason));
    }

    output.push_str(&format!("Next Prompt:\n{}", context.next_prompt));

    Ok(output)
}

// ── simulate_change ────────────────────────────────────────────────────────────

fn tool_simulate_change(args: &Value, store: &Store) -> Result<String, String> {
    let item_name = args["item"].as_str().ok_or("missing `item`")?;
    let change_description = args["change"].as_str().unwrap_or("unspecified change");
    let depth = args["depth"].as_u64().unwrap_or(1) as u8;

    let result = if depth > 1 {
        crate::reasoner::simulator::simulate_change_deep(
            store.conn(),
            item_name,
            change_description,
            depth,
        )
    } else {
        crate::reasoner::simulator::simulate_change(
            store.conn(),
            item_name,
            change_description,
        )
    }.map_err(|e| format!("Simulation failed: {}", e))?;

    Ok(result.render())
}
