use std::collections::{HashMap, HashSet, VecDeque};
use std::path::Path;

use rusqlite::{params, Connection, OptionalExtension};
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
        "semantic_search"      => tool_semantic_search(args, store, units, sessions, session_id),
        "get_item"             => tool_get_item(args, store, units, sessions, session_id),
        "get_syntax"           => tool_get_syntax(args, store, units, session_id),
        "get_usage_examples"   => tool_get_usage_examples(args, store, units, session_id),
        "get_helper"           => tool_get_helper(args, store, units, session_id),
        "get_context"          => tool_get_context(args, store, units, repo_root, prefs_summary),
        "get_delta"            => tool_get_delta(args, repo_root),
        "query_graph"          => tool_query_graph(args, store, session_id),
        "explain_dependency_path" => tool_explain_dependency_path(args, store),
        "get_preferences"      => tool_get_preferences(prefs_summary),
        "recurrent_think"      => tool_recurrent_think(args, store),
        "simulate_change"      => tool_simulate_change(args, store),
        "recall"               => tool_recall(args, store, units, sessions, session_id),
        "list_patterns"        => tool_list_patterns(args, store, session_id),
        "get_anti_patterns"    => tool_get_anti_patterns(store, session_id),
        "suggest_pattern"      => tool_suggest_pattern(args, store),
        "list_all"             => tool_list_all(args, units),
        // Phase 0B: protocol session management tools.
        "begin_protocol_session" => tool_begin_protocol_session(args, store, session_id),
        "get_session_health"     => tool_get_session_health(store, session_id),
        // Phase 0C/0D: knowledge capture tools.
        "flush_knowledge_markers" => tool_flush_knowledge_markers(store, session_id, repo_root),
        "closeout_session"        => tool_closeout_session(args, store, session_id, repo_root),
        // Phase 1: skill proposal tool.
        "propose_skill"           => tool_propose_skill(args, store, session_id, repo_root),
        other                  => Err(format!("unknown tool: {other}")),
    }?;

    Ok(json!({ "content": [{ "type": "text", "text": text }] }))
}

// ── semantic_search ───────────────────────────────────────────────────────────

fn tool_semantic_search(
    args: &Value,
    store: &Store,
    units: &[CodeUnit],
    sessions: &SessionRegistry,
    session_id: &str,
) -> Result<String, String> {
    let query = args["query"].as_str().ok_or("missing `query`")?;
    let limit = args["limit"].as_u64().unwrap_or(5) as usize;

    let results = semantic_search(query, units, limit);
    let keyword = keyword_search(query, units);

    if results.is_empty() && keyword.is_empty() {
        let _ = store.log_query_gap(
            "semantic_search",
            query,
            Some(session_id),
            Some("no semantic or keyword matches"),
        );
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
    store: &Store,
    units: &[CodeUnit],
    sessions: &SessionRegistry,
    session_id: &str,
) -> Result<String, String> {
    let name = args["name"].as_str().ok_or("missing `name`")?;

    let unit = units.iter().find(|u| u.name == name)
        .ok_or_else(|| {
            let _ = store.log_query_gap(
                "get_item",
                name,
                Some(session_id),
                Some("no indexed item with exact name"),
            );
            format!("no item named `{name}`")
        })?;

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

fn tool_get_syntax(
    args: &Value,
    store: &Store,
    units: &[CodeUnit],
    session_id: &str,
) -> Result<String, String> {
    let symbol = args["symbol_name"].as_str().ok_or("missing `symbol_name`")?;
    let candidate = find_symbol_unit(symbol, units);

    let Some(unit) = candidate else {
        let suggestions = similar_symbol_units(symbol, units, 5);

        if suggestions.is_empty() {
            let _ = store.log_query_gap(
                "get_syntax",
                symbol,
                Some(session_id),
                Some("no symbol match or similar suggestions"),
            );
            return Err(format!("no symbol found for `{symbol}`"));
        }

        let _ = store.log_query_gap(
            "get_syntax",
            symbol,
            Some(session_id),
            Some("no exact symbol match; only similar suggestions"),
        );

        let names = suggestions
            .iter()
            .map(|u| format!("{} ({})", u.name, u.module_path))
            .collect::<Vec<_>>()
            .join(" | ");
        return Err(format!("no exact symbol found for `{symbol}`. Similar: {names}"));
    };

    let mut sig = String::new();
    let mut fields = String::new();
    let mut methods = String::new();
    let mut variants = Vec::new();

    for line in unit.compressed.lines() {
        let trimmed = line.trim();
        if let Some(v) = trimmed.strip_prefix("sig:") {
            sig = v.trim().to_string();
        } else if let Some(v) = trimmed.strip_prefix("fields:") {
            fields = v.trim().to_string();
        } else if let Some(v) = trimmed.strip_prefix("methods:") {
            methods = v.trim().to_string();
        } else if trimmed.starts_with(&format!("{}::", unit.name)) {
            variants.push(trimmed.to_string());
        }
    }

    let mut out = String::new();
    out.push_str(&format!("Symbol: {}\nKind: {}\nModule: {}\n", unit.name, unit.kind, unit.module_path));

    if !sig.is_empty() {
        out.push_str(&format!("\nSignature\n{}\n", sig));
    }
    if !fields.is_empty() {
        out.push_str(&format!("\nFields\n{}\n", fields));
    }
    if !methods.is_empty() {
        out.push_str(&format!("\nMethods\n{}\n", methods));
    }
    if !variants.is_empty() {
        out.push_str("\nVariants\n");
        for v in variants.iter().take(20) {
            out.push_str(&format!("- {}\n", v));
        }
    }

    if sig.is_empty() && fields.is_empty() && methods.is_empty() && variants.is_empty() {
        out.push_str("\nNo structured signature details found. Use get_item for full compressed entry.");
    }

    Ok(out)
}

fn tool_get_usage_examples(
    args: &Value,
    store: &Store,
    units: &[CodeUnit],
    session_id: &str,
) -> Result<String, String> {
    let symbol = args["symbol_name"].as_str().ok_or("missing `symbol_name`")?;
    let tier = args.get("tier").and_then(|v| v.as_str());
    let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(5) as usize;

    let db_examples = store
        .get_symbol_examples(symbol, tier, limit)
        .map_err(|e| e.to_string())?;

    let mut out = String::new();
    out.push_str(&format!("Usage examples for `{}`\n\n", symbol));

    if !db_examples.is_empty() {
        out.push_str("From symbol_examples\n");
        for (idx, (file, line, snippet, source_tier)) in db_examples.iter().enumerate() {
            out.push_str(&format!(
                "{}. {}{} [{}]\n",
                idx + 1,
                file,
                line.map(|n| format!(":{}", n)).unwrap_or_default(),
                source_tier,
            ));
            let compact = snippet.lines().take(16).collect::<Vec<_>>().join("\n");
            out.push_str(&format!("{}\n\n", compact));
        }
        return Ok(out);
    }

    let fallback = units
        .iter()
        .filter(|u| {
            u.id == symbol
                || u.name == symbol
                || u.id.ends_with(&format!("::{}", symbol))
                || u.name.eq_ignore_ascii_case(symbol)
                || u.compressed.contains(symbol)
        })
        .take(limit)
        .collect::<Vec<_>>();

    if fallback.is_empty() {
        let _ = store.log_query_gap(
            "get_usage_examples",
            symbol,
            Some(session_id),
            Some("no symbol_examples rows and no indexed fallback units"),
        );
        return Err(format!("no usage examples found for `{}`", symbol));
    }

    out.push_str("Fallback from indexed units\n");
    for (idx, unit) in fallback.iter().enumerate() {
        out.push_str(&format!(
            "{}. {} ({})\n",
            idx + 1,
            unit.id,
            unit.module_path
        ));
        let compact = unit.compressed.lines().take(16).collect::<Vec<_>>().join("\n");
        out.push_str(&format!("{}\n\n", compact));
    }

    Ok(out)
}

fn tool_get_helper(
    args: &Value,
    store: &Store,
    units: &[CodeUnit],
    session_id: &str,
) -> Result<String, String> {
    let symbol = args["symbol_name"].as_str().ok_or("missing `symbol_name`")?;
    let intent = args.get("intent").and_then(|v| v.as_str()).unwrap_or("");
    let intent_lower = intent.to_lowercase();

    let catalog = store
        .get_symbol_catalog_entry(symbol)
        .map_err(|e| e.to_string())?;
    let unit = find_symbol_unit(symbol, units);

    let (resolved_name, kind, module_path, signature, helper_tags) = if let Some(c) = catalog {
        (c.0, c.1, c.2, c.3.unwrap_or_default(), c.5)
    } else if let Some(u) = unit {
        (
            u.id.clone(),
            u.kind.clone(),
            u.module_path.clone(),
            u.summary.clone(),
            u.name.clone(),
        )
    } else {
        let similar = store
            .find_symbol_catalog_similar(symbol, 5)
            .map_err(|e| e.to_string())?;
        if similar.is_empty() {
            let _ = store.log_query_gap(
                "get_helper",
                symbol,
                Some(session_id),
                Some("no catalog entry and no similar symbols"),
            );
            return Err(format!("no helper guidance found for `{}`", symbol));
        }
        let _ = store.log_query_gap(
            "get_helper",
            symbol,
            Some(session_id),
            Some("no exact symbol in catalog; returned similar hints"),
        );
        let hints = similar
            .iter()
            .map(|(n, k, m)| format!("{} ({}, {})", n, k, m))
            .collect::<Vec<_>>()
            .join(" | ");
        return Err(format!("no exact symbol for `{}`. Similar: {}", symbol, hints));
    };

    let mut guidance: Vec<String> = Vec::new();

    if kind == "enum" {
        guidance.push("Use `get_syntax` first to confirm exact variant names before wiring logic.".to_string());
    }
    if resolved_name.contains("Action") {
        guidance.push("Dispatch through `canvas.run(Action::...)` instead of direct mutation paths.".to_string());
    }
    if resolved_name.contains("Condition") {
        guidance.push("Prefer declarative branching with `Action::Conditional` over ad-hoc if/match in update loops.".to_string());
    }
    if resolved_name.contains("Target") || resolved_name.contains("Location") {
        guidance.push("Use constructor helpers rather than tuple-style enum assumptions for target/location values.".to_string());
    }
    if kind == "fn" || kind == "method" {
        guidance.push("Pull usage snippets with `get_usage_examples` to verify common call shapes.".to_string());
    }

    if intent_lower.contains("refactor") || intent_lower.contains("change") {
        guidance.push("Run `simulate_change` before edits to estimate blast radius and test scope.".to_string());
    }
    if intent_lower.contains("safe") || intent_lower.contains("pitfall") {
        guidance.push("Check `get_anti_patterns` before coding to avoid known regressions.".to_string());
    }
    if intent_lower.contains("example") || intent_lower.contains("usage") {
        guidance.push("Call `get_usage_examples` for concrete, local callsite references.".to_string());
    }

    if !helper_tags.trim().is_empty() {
        guidance.push(format!("Related helper tags: {}", helper_tags));
    }

    if guidance.is_empty() {
        guidance.push("No special helper heuristics matched; use get_syntax + get_usage_examples for the safest path.".to_string());
    }

    let mut out = String::new();
    out.push_str(&format!(
        "Helper guidance for `{}`\nKind: {}\nModule: {}\n",
        resolved_name, kind, module_path
    ));
    if !signature.trim().is_empty() {
        out.push_str(&format!("Signature hint: {}\n", signature));
    }
    out.push_str("\nRecommendations\n");
    for (idx, g) in guidance.iter().enumerate() {
        out.push_str(&format!("{}. {}\n", idx + 1, g));
    }

    Ok(out)
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
        let _ = store.log_query_gap(
            "get_context",
            hint,
            None,
            Some("context packet resolved empty across units/patterns/anti-patterns/annotations"),
        );
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

fn tool_query_graph(args: &Value, store: &Store, session_id: &str) -> Result<String, String> {
    let name = args["name"].as_str().ok_or("missing `name`")?;
    let depth = args["depth"].as_u64().unwrap_or(1) as u8;

    let unit = store.get_unit(name).map_err(|e| e.to_string())?;
    let Some(root) = unit else {
        let _ = store.log_query_gap(
            "query_graph",
            name,
            Some(session_id),
            Some("no graph root node found"),
        );
        return Ok(format!("No graph node found for `{}`", name));
    };

    let (edges, nodes) = graph::subgraph(store.conn(), &root.id, depth).map_err(|e| e.to_string())?;
    if edges.is_empty() {
        let _ = store.log_query_gap(
            "query_graph",
            name,
            Some(session_id),
            Some("graph root has no neighbors for requested depth"),
        );
        return Ok(format!("{} ({}): no graph neighbors", root.name, root.id));
    }

    // Build id → name map from returned nodes for human-readable output.
    let node_name: std::collections::HashMap<&str, &str> = nodes.iter()
        .map(|n| (n.id.as_str(), n.name.as_str()))
        .collect();

    let mut out = String::new();
    out.push_str(&format!("{} ({}) -> {} relations:\n", root.name, root.kind, edges.len()));
    for e in &edges {
        let target_name = node_name.get(e.to_id.as_str()).copied().unwrap_or(&e.to_id);
        out.push_str(&format!("  -[{}]-> {} ({})\n", e.relation.as_str(), target_name, e.to_id));
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
            if let Some(id) = p.id {
                let _ = store.pattern_used(id);
                let _ = store.log_session_retrieval(session_id, "patterns", id, "recall");
            }
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
            if let Some(id) = ap.id {
                let _ = store.log_session_retrieval(session_id, "anti_patterns", id, "recall");
            }
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
            if let Some(id) = a.id {
                let _ = store.log_session_retrieval(session_id, "annotations", id, "recall");
            }
        }
    }

    if !found {
        let _ = store.log_query_gap(
            "recall",
            topic,
            Some(session_id),
            Some("no matching api units, patterns, anti-patterns, or annotations"),
        );
        out.push_str("Nothing found. Consider adding an annotation.\n");
    }

    Ok(out)
}

// ── list_patterns ─────────────────────────────────────────────────────────────

fn tool_list_patterns(args: &Value, store: &Store, session_id: &str) -> Result<String, String> {
    let patterns = store.all_patterns().map_err(|e| e.to_string())?;
    if patterns.is_empty() {
        return Ok("No approved patterns yet.".into());
    }

    let detail = list_detail_tier(args);
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
        if let Some(id) = p.id {
            let _ = store.log_session_retrieval(session_id, "patterns", id, "list_patterns");
        }

        if detail != "summary" {
            if !p.uses.is_empty() {
                out.push_str(&format!("Uses: {}\n", p.uses.join(", ")));
            }
        }

        if detail == "full" {
            out.push_str(&p.body);
            out.push('\n');
        } else if detail == "standard" {
            let preview = p.body.lines().take(4).collect::<Vec<_>>().join("\n");
            if !preview.trim().is_empty() {
                out.push_str(&preview);
                out.push('\n');
            }
        }

        out.push('\n');
    }
    Ok(out)
}

// ── get_anti_patterns ─────────────────────────────────────────────────────────

fn tool_get_anti_patterns(store: &Store, session_id: &str) -> Result<String, String> {
    let aps = store.all_anti_patterns().map_err(|e| e.to_string())?;
    if aps.is_empty() {
        return Ok("No anti-patterns recorded yet.".into());
    }
    let mut out = format!("{} anti-pattern(s) — DO NOT do these:\n\n", aps.len());
    for ap in &aps {
        out.push_str(&format!("### {}\n✗ wrong:   {}\n✓ correct: {}\n\n",
            ap.description, ap.wrong, ap.correct));
        if let Some(id) = ap.id {
            let _ = store.log_session_retrieval(session_id, "anti_patterns", id, "get_anti_patterns");
        }
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
    let detail = list_detail_tier(args);

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
            match detail {
                "summary" => {
                    out.push_str(&format!("- `{}`\n", u.name));
                }
                "full" => {
                    out.push_str(&format!("- `{}` — {}\n", u.name, u.summary));
                    let preview = u.compressed.lines().take(8).collect::<Vec<_>>().join("\n");
                    if !preview.trim().is_empty() {
                        out.push_str(&format!("{}\n", preview));
                    }
                }
                _ => {
                    out.push_str(&format!("- `{}` — {}\n", u.name, u.summary));
                }
            }
        }
        out.push('\n');
    }

    Ok(out)
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn list_detail_tier(args: &Value) -> &str {
    match args.get("detail").and_then(|v| v.as_str()) {
        Some("summary") => "summary",
        Some("full") => "full",
        _ => "standard",
    }
}

fn find_symbol_unit<'a>(symbol: &str, units: &'a [CodeUnit]) -> Option<&'a CodeUnit> {
    let exact_id = units.iter().find(|u| u.id == symbol);
    let exact_name = units.iter().find(|u| u.name == symbol);
    exact_id
        .or(exact_name)
        .or_else(|| units.iter().find(|u| u.id.ends_with(&format!("::{symbol}"))))
        .or_else(|| units.iter().find(|u| u.name.eq_ignore_ascii_case(symbol)))
}

fn similar_symbol_units<'a>(symbol: &str, units: &'a [CodeUnit], limit: usize) -> Vec<&'a CodeUnit> {
    let symbol_lower = symbol.to_lowercase();
    let mut suggestions: Vec<&CodeUnit> = units
        .iter()
        .filter(|u| u.name.to_lowercase().contains(&symbol_lower))
        .take(limit)
        .collect();
    suggestions.sort_by(|a, b| a.name.cmp(&b.name));
    suggestions
}

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
    let session_key = args["session_key"].as_str();
    let max_loops = match depth_mode {
        "shallow" => 2u8,
        "deep" => args["max_loops"].as_u64().unwrap_or(12).min(16) as u8,
        _ => args["max_loops"].as_u64().unwrap_or(6).min(16) as u8,
    };

    // Load persisted scratchpad from SQLite or initialize a new one.
    // session_key scopes the scratchpad — different sessions with the same task
    // get distinct scratchpads (Phase 4).
    let mut scratchpad =
        crate::reasoner::scratchpad::load_from_db(store.conn(), task, session_key)
            .map_err(|e| format!("Failed to load scratchpad: {}", e))?
            .unwrap_or_else(|| crate::reasoner::scratchpad::Scratchpad::new(task, session_key));

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

fn tool_explain_dependency_path(args: &Value, store: &Store) -> Result<String, String> {
    let from = args["from"].as_str().ok_or("missing `from`")?;
    let to = args["to"].as_str().ok_or("missing `to`")?;
    let max_depth = args.get("depth").and_then(|v| v.as_u64()).unwrap_or(4) as usize;

    let from_candidates = resolve_graph_candidates(store.conn(), from, 6).map_err(|e| e.to_string())?;
    let to_candidates = resolve_graph_candidates(store.conn(), to, 6).map_err(|e| e.to_string())?;

    if from_candidates.is_empty() {
        return Err(format!("no graph node found for `from`: {}", from));
    }
    if to_candidates.is_empty() {
        return Err(format!("no graph node found for `to`: {}", to));
    }

    let target_ids: HashSet<String> = to_candidates.iter().map(|(id, _, _)| id.clone()).collect();

    let mut found: Option<(String, Vec<(String, String, String)>)> = None;
    let mut chosen_start: Option<(String, String, String)> = None;
    for start in &from_candidates {
        if let Some(path) = bfs_dependency_path(store.conn(), &start.0, &target_ids, max_depth)? {
            chosen_start = Some(start.clone());
            found = Some(path);
            break;
        }
    }

    let mut out = String::new();
    out.push_str(&format!(
        "Dependency path query: from `{}` to `{}` (depth <= {})\n\n",
        from, to, max_depth
    ));

    if from_candidates.len() > 1 {
        let labels = from_candidates
            .iter()
            .map(|(id, name, module)| format!("{} [{} | {}]", id, name, module))
            .collect::<Vec<_>>()
            .join(" | ");
        out.push_str(&format!("From candidates: {}\n", labels));
    }
    if to_candidates.len() > 1 {
        let labels = to_candidates
            .iter()
            .map(|(id, name, module)| format!("{} [{} | {}]", id, name, module))
            .collect::<Vec<_>>()
            .join(" | ");
        out.push_str(&format!("To candidates: {}\n", labels));
    }

    let Some((target_id, path_steps)) = found else {
        out.push_str("No dependency path found within depth limit.");
        return Ok(out);
    };

    if let Some((start_id, start_name, _)) = chosen_start {
        out.push_str(&format!("\nResolved start: {} ({})\n", start_id, start_name));
    }
    if let Some((_, target_name, _)) = to_candidates.iter().find(|(id, _, _)| *id == target_id) {
        out.push_str(&format!("Resolved target: {} ({})\n", target_id, target_name));
    } else {
        out.push_str(&format!("Resolved target: {}\n", target_id));
    }

    out.push_str("\nPath\n");
    for (idx, (from_id, relation, to_id)) in path_steps.iter().enumerate() {
        out.push_str(&format!("{}. {} -[{}]-> {}\n", idx + 1, from_id, relation, to_id));
    }

    Ok(out)
}

fn tool_simulate_change(args: &Value, store: &Store) -> Result<String, String> {
    let item_name = args["item"].as_str().ok_or("missing `item`")?;
    let change_description = args["change"].as_str().unwrap_or("unspecified change");
    let depth = args["depth"].as_u64().unwrap_or(1) as u8;
    let relation_filter = parse_relation_filter(args.get("relation_filter"));

    let mut result = if depth > 1 {
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

    if let Some(filter) = relation_filter {
        result
            .affected
            .retain(|a| filter.contains(&a.relation.to_lowercase()));
        result
            .depends_on
            .retain(|a| filter.contains(&a.relation.to_lowercase()));
        result.risk_level = classify_risk(result.affected.len(), result.depends_on.len());

        if result.affected.is_empty() && result.depends_on.is_empty() {
            result.warnings.push(
                "relation_filter removed all matches; broaden filter or omit it for full impact".to_string(),
            );
        }
    }

    Ok(result.render())
}

fn parse_relation_filter(v: Option<&Value>) -> Option<HashSet<String>> {
    let mut set = HashSet::new();
    match v {
        Some(Value::String(s)) => {
            let normalized = s.trim().to_lowercase();
            if !normalized.is_empty() {
                set.insert(normalized);
            }
        }
        Some(Value::Array(items)) => {
            for it in items {
                if let Some(s) = it.as_str() {
                    let normalized = s.trim().to_lowercase();
                    if !normalized.is_empty() {
                        set.insert(normalized);
                    }
                }
            }
        }
        _ => {}
    }

    if set.is_empty() { None } else { Some(set) }
}

fn classify_risk(affected_len: usize, depends_len: usize) -> crate::reasoner::simulator::RiskLevel {
    let basis = affected_len + (depends_len / 2);
    match basis {
        0..=2 => crate::reasoner::simulator::RiskLevel::Low,
        3..=7 => crate::reasoner::simulator::RiskLevel::Medium,
        _ => crate::reasoner::simulator::RiskLevel::High,
    }
}

fn resolve_graph_candidates(
    conn: &Connection,
    name_or_id: &str,
    limit: usize,
) -> rusqlite::Result<Vec<(String, String, String)>> {
    let mut out: Vec<(String, String, String)> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();

    if let Some(row) = conn
        .query_row(
            "SELECT id, name, module_path FROM graph_nodes WHERE id = ?1 LIMIT 1",
            [name_or_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )
        .optional()?
    {
        seen.insert(row.0.clone());
        out.push(row);
    }

    if out.len() >= limit {
        return Ok(out);
    }

    let mut stmt = conn.prepare(
        "SELECT id, name, module_path FROM graph_nodes
         WHERE name = ?1 OR lower(name) = lower(?1)
         ORDER BY module_path
         LIMIT ?2"
    )?;
    let exact_rows = stmt.query_map(params![name_or_id, limit as i64], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
        ))
    })?;
    for r in exact_rows {
        let tup = r?;
        if seen.insert(tup.0.clone()) {
            out.push(tup);
            if out.len() >= limit {
                return Ok(out);
            }
        }
    }

    let like = format!("%{}%", name_or_id);
    let mut stmt = conn.prepare(
        "SELECT id, name, module_path FROM graph_nodes
         WHERE id LIKE ?1 OR name LIKE ?1
         ORDER BY module_path
         LIMIT ?2"
    )?;
    let fuzzy_rows = stmt.query_map(params![like, limit as i64], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
        ))
    })?;
    for r in fuzzy_rows {
        let tup = r?;
        if seen.insert(tup.0.clone()) {
            out.push(tup);
            if out.len() >= limit {
                break;
            }
        }
    }

    Ok(out)
}

fn bfs_dependency_path(
    conn: &Connection,
    start_id: &str,
    target_ids: &HashSet<String>,
    max_depth: usize,
) -> Result<Option<(String, Vec<(String, String, String)>)>, String> {
    if target_ids.contains(start_id) {
        return Ok(Some((start_id.to_string(), Vec::new())));
    }

    let mut queue: VecDeque<(String, usize)> = VecDeque::new();
    let mut visited: HashSet<String> = HashSet::new();
    let mut parent: HashMap<String, (String, String)> = HashMap::new();

    queue.push_back((start_id.to_string(), 0));
    visited.insert(start_id.to_string());

    while let Some((current, depth)) = queue.pop_front() {
        if depth >= max_depth {
            continue;
        }

        let neighbors = crate::graph::neighbors(conn, &current).map_err(|e| e.to_string())?;

        for (edge, node) in neighbors {
            if !visited.insert(node.id.clone()) {
                continue;
            }

            parent.insert(
                node.id.clone(),
                (current.clone(), edge.relation.as_str().to_string()),
            );

            if target_ids.contains(&node.id) {
                let mut steps_rev: Vec<(String, String, String)> = Vec::new();
                let mut cursor = node.id.clone();
                while cursor != start_id {
                    if let Some((p, rel)) = parent.get(&cursor).cloned() {
                        steps_rev.push((p.clone(), rel, cursor.clone()));
                        cursor = p;
                    } else {
                        break;
                    }
                }
                steps_rev.reverse();
                return Ok(Some((node.id, steps_rev)));
            }

            queue.push_back((node.id.clone(), depth + 1));
        }
    }

    Ok(None)
}

// ── Phase 0B: protocol session tools ─────────────────────────────────────────

/// begin_protocol_session — activate PROTOCOL mode and return session health.
fn tool_begin_protocol_session(
    args: &Value,
    store: &Store,
    session_id: &str,
) -> Result<String, String> {
    let task = args.get("task").and_then(|v| v.as_str()).unwrap_or("unspecified task");

    // Activate PROTOCOL mode for this session.
    crate::protocol::activate_protocol_mode(store.conn(), session_id)
        .map_err(|e| e.to_string())?;

    let gaps = crate::protocol::top_query_gaps(store.conn(), 3)
        .unwrap_or_default();
    let health = crate::protocol::pattern_health_summary(store.conn())
        .unwrap_or_default();
    let markers = crate::protocol::session_marker_counts(store.conn(), session_id)
        .unwrap_or((0, 0, 0));
    let pending_obs = store.all_observations()
        .map(|v| v.len())
        .unwrap_or(0);
    let pending_proposals = crate::protocol::pending_proposal_count(store.conn())
        .unwrap_or(0);

    let mut report = crate::protocol::status_report(
        store.conn(),
        session_id,
        pending_obs,
        Some(markers),
        &gaps,
        &health,
        pending_proposals,
    ).map_err(|e| e.to_string())?;

    report.insert_str(0, &format!(
        "PROTOCOL session started — task: \"{}\"\n\n",
        task.chars().take(120).collect::<String>()
    ));
    report.push_str("\n\nPhase 0 required: call get_delta → get_preferences → get_anti_patterns → get_context\nWork tools are gated until Phase 0 is complete.");

    Ok(report)
}

/// get_session_health — one-call session status report.
fn tool_get_session_health(
    store: &Store,
    session_id: &str,
) -> Result<String, String> {
    let gaps = crate::protocol::top_query_gaps(store.conn(), 3)
        .unwrap_or_default();
    let health = crate::protocol::pattern_health_summary(store.conn())
        .unwrap_or_default();
    let markers = crate::protocol::session_marker_counts(store.conn(), session_id)
        .unwrap_or((0, 0, 0));
    let pending_obs = store.all_observations()
        .map(|v| v.len())
        .unwrap_or(0);
    let pending_proposals = crate::protocol::pending_proposal_count(store.conn())
        .unwrap_or(0);

    crate::protocol::status_report(
        store.conn(),
        session_id,
        pending_obs,
        Some(markers),
        &gaps,
        &health,
        pending_proposals,
    ).map_err(|e| e.to_string())
}

// ── Phase 0C/0D: knowledge capture tools ─────────────────────────────────────

/// flush_knowledge_markers — scan VS Code session store, extract CORTEX-* tags, stage them.
fn tool_flush_knowledge_markers(
    store: &Store,
    session_id: &str,
    repo_root: &Path,
) -> Result<String, String> {
    let store_path = crate::session_store::find_session_store();
    let Some(path) = store_path else {
        return Ok("VS Code session store not found. Ensure VS Code is installed and has been used. No markers extracted.".to_string());
    };

    let conn = crate::session_store::open_readonly(&path)
        .map_err(|e| e.to_string())?;
    let responses = crate::session_store::recent_assistant_responses(&conn, 60)
        .unwrap_or_default();

    if responses.is_empty() {
        return Ok("No recent assistant responses found in session store. No markers extracted.".to_string());
    }

    let all_text = responses.join("\n\n---\n\n");
    let parsed = crate::markers::parse_markers(&all_text);

    if parsed.is_empty() {
        return Ok("No CORTEX-* markers found in recent responses. Write markers like [CORTEX-PATTERN: ...] to capture knowledge.".to_string());
    }

    let mut staged = 0usize;
    for marker in &parsed {
        let body = match marker {
            crate::markers::KnowledgeMarker::Pattern { body, .. } => body.clone(),
            crate::markers::KnowledgeMarker::AntiPattern { description, wrong, correct, .. } =>
                format!("{description}\nwrong: {wrong}\ncorrect: {correct}"),
            crate::markers::KnowledgeMarker::Correction { attempted, reason, fix, .. } =>
                format!("attempted: {attempted}\nreason: {reason}\nfix: {fix}"),
            crate::markers::KnowledgeMarker::Adr { context, decision, .. } =>
                format!("Context: {context}\nDecision: {decision}"),
            crate::markers::KnowledgeMarker::PrefsNote { body, .. } => body.clone(),
            crate::markers::KnowledgeMarker::SkillCandidate { summary, .. } => summary.clone(),
        };
        let name = marker.display_name();
        let tags_json = "[]".to_string();
        let trust = if let crate::markers::KnowledgeMarker::Pattern { trust, .. } = marker {
            trust.clone()
        } else { "annotated".to_string() };

        let _ = store.conn().execute(
            "INSERT INTO knowledge_markers
                 (session_key, marker_type, name, body, tags, trust_level, raw_tag, promoted)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, '', 0)",
            rusqlite::params![session_id, marker.marker_type(), name, body, tags_json, trust],
        );
        staged += 1;
    }

    let _ = store.conn().execute(
        "UPDATE protocol_sessions SET knowledge_markers_flushed = 1 WHERE session_key = ?1",
        rusqlite::params![session_id],
    );

    let mut out = format!("Extracted {} marker(s) from recent responses:\n", staged);
    for m in &parsed {
        out.push_str(&format!("  [{}] {}\n", m.marker_type(), m.display_name()));
    }
    out.push_str("\nMarkers staged (not yet committed). Call closeout_session(inline_approve=true) with KNOWLEDGE COMMITTED to commit them.");
    Ok(out)
}

/// closeout_session — the single-call session closeout replacing the 7-step checklist.
fn tool_closeout_session(
    args: &Value,
    store: &Store,
    session_id: &str,
    repo_root: &Path,
) -> Result<String, String> {
    let outcome_type = args["outcome_type"].as_str().ok_or("missing `outcome_type`")?;
    let inline_approve = args.get("inline_approve")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let error_text   = args.get("error_text").and_then(|v| v.as_str());
    let diff_symbols = args.get("diff_symbols").and_then(|v| v.as_str());

    // Determine prefs path from repo_root.
    let prefs_path = repo_root.join(".cortex").join("prefs.toml");
    let prefs_path_opt = if prefs_path.exists() { Some(prefs_path.as_path()) } else { None };

    let result = crate::closeout::run_closeout(
        store,
        session_id,
        outcome_type,
        error_text,
        diff_symbols,
        inline_approve,
        repo_root,
        prefs_path_opt,
    ).map_err(|e| e.to_string())?;

    // Build response.
    let mut out = String::new();

    if inline_approve {
        out.push_str("✓ KNOWLEDGE COMMITTED — session closed.\n\n");
        out.push_str("Committed to Cortex DB:\n");
        out.push_str(&format!("  {} patterns\n", result.patterns_committed));
        out.push_str(&format!("  {} anti-patterns\n", result.anti_patterns_committed));
        out.push_str(&format!("  {} corrections\n", result.corrections_committed));
        out.push_str(&format!("  {} ADRs\n", result.adrs_committed));
        out.push_str(&format!("  {} prefs notes\n", result.prefs_notes_committed));
        if result.skill_candidates_staged > 0 {
            out.push_str(&format!("  {} skill candidates staged for Tier 2 consolidation\n",
                result.skill_candidates_staged));
        }
    } else {
        out.push_str("Session closed (staged mode).\n\n");
        if result.markers_staged > 0 {
            out.push_str(&format!("{} markers staged (not committed).\n", result.markers_staged));
            out.push_str("To commit, call closeout_session again with inline_approve=true after user types KNOWLEDGE COMMITTED.\n");
        } else {
            out.push_str("No markers found in recent responses. Write CORTEX-* markers to capture knowledge next session.\n");
        }
    }

    out.push_str(&format!("\nOutcome logged: {} ({})\n", outcome_type,
        if result.outcome_logged { "✓" } else { "failed" }));

    if result.graph_snapshot_written {
        out.push_str("Graph snapshot: ✓ written\n");
    }
    if result.session_snapshot_written {
        out.push_str("Session snapshot: ✓ written to .cortex/mined-tasks/\n");
    }
    if result.mirror_written {
        out.push_str("Mirror: ✓ written to .agent-memory/mirrors/repo/\n");
    }

    Ok(out)
}

// ── Phase 1: propose_skill ────────────────────────────────────────────────────

/// propose_skill — agent-initiated skill proposal.
fn tool_propose_skill(
    args: &Value,
    store: &Store,
    _session_id: &str,
    repo_root: &Path,
) -> Result<String, String> {
    let name      = args["name"].as_str().ok_or("missing `name`")?;
    let trigger   = args.get("trigger").and_then(|v| v.as_str()).unwrap_or("");
    let procedure = args["procedure"].as_str().ok_or("missing `procedure`")?;
    let tools_str = args.get("tools").and_then(|v| v.as_str()).unwrap_or("");

    let tool_sequence: Vec<String> = if tools_str.is_empty() {
        vec![]
    } else {
        tools_str.split(',').map(|s| s.trim().to_string()).collect()
    };

    let proposals_dir = repo_root.join(".cortex").join("proposals");
    let prefs_path    = repo_root.join(".cortex").join("prefs.toml");
    let skills_dir    = crate::prefs::load(&prefs_path)
        .map(|p| p.skills.skills_dir)
        .unwrap_or_else(|_| "agent_customization/skills".to_string());

    let confidence = 0.8f32; // agent-proposed; assume high until evidence says otherwise

    match crate::skills::draft_skill_file(
        name, &tool_sequence, 1, confidence, &proposals_dir, &skills_dir,
    ) {
        Ok(path) => {
            let _ = crate::skills::set_skill_draft_path(store, name, &path);
            Ok(format!(
                "Skill draft written: {path}\n\
                 Name: {name}\n\
                 Trigger: {trigger}\n\
                 To publish: cortex.ps1 skill-approve {name}\n\
                 The draft is in .cortex/proposals/ — review and edit before approving."
            ))
        }
        Err(e) => Err(format!("failed to draft skill '{name}': {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::{classify_risk, parse_relation_filter};
    use crate::reasoner::simulator::RiskLevel;
    use serde_json::json;

    #[test]
    fn parse_relation_filter_supports_single_string() {
        let parsed = parse_relation_filter(Some(&json!("uses"))).expect("expected filter set");
        assert!(parsed.contains("uses"));
        assert_eq!(parsed.len(), 1);
    }

    #[test]
    fn parse_relation_filter_supports_string_arrays_and_normalizes() {
        let parsed = parse_relation_filter(Some(&json!(["Calls", " uses ", "", null]))).expect("expected filter set");
        assert!(parsed.contains("calls"));
        assert!(parsed.contains("uses"));
        assert_eq!(parsed.len(), 2);
    }

    #[test]
    fn parse_relation_filter_rejects_empty_inputs() {
        assert!(parse_relation_filter(Some(&json!("   "))).is_none());
        assert!(parse_relation_filter(Some(&json!([]))).is_none());
        assert!(parse_relation_filter(None).is_none());
    }

    #[test]
    fn classify_risk_thresholds_are_stable() {
        assert_eq!(classify_risk(0, 0), RiskLevel::Low);
        assert_eq!(classify_risk(2, 0), RiskLevel::Low);
        assert_eq!(classify_risk(3, 0), RiskLevel::Medium);
        assert_eq!(classify_risk(6, 2), RiskLevel::Medium);
        assert_eq!(classify_risk(8, 0), RiskLevel::High);
        assert_eq!(classify_risk(7, 4), RiskLevel::High);
    }
}
