pub mod tools;

use std::path::PathBuf;
use std::io::{BufRead, Write};

use anyhow::Result;
use serde_json::{json, Value};

use crate::cache::{
    cache_stats, compute_index_version, get_cached_response, cache_response,
    invalidate_stale, SessionRegistry,
};
use crate::memory::Store;
use crate::model::CodeUnit;

/// Max response cache entries before LRU eviction kicks in.
const CACHE_MAX_ENTRIES: usize = 256;

/// Tools that are never cached (always need live data).
const UNCACHEABLE: &[&str] = &[
    "suggest_pattern",
    "list_patterns",
    "get_anti_patterns",
    "get_delta",
    "recurrent_think",
    "simulate_change",
];

pub fn serve(
    store: Store,
    units: Vec<CodeUnit>,
    engine_name: &str,
    repo_root: PathBuf,
    prefs_summary: String,
) -> Result<()> {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut out = std::io::BufWriter::new(stdout.lock());
    let engine_name = engine_name.to_string();
    let sessions = SessionRegistry::new();

    // Compute index version once at startup; changes only after `cortex index`.
    let index_version = compute_index_version(store.conn())
        .unwrap_or_else(|_| "unknown".to_string());

    // Flush stale cache entries from previous index versions.
    let flushed = invalidate_stale(store.conn(), &index_version).unwrap_or(0);
    if flushed > 0 {
        eprintln!("  cache: flushed {} stale entries (index version changed)", flushed);
    }

    let stats = cache_stats(store.conn()).ok();
    if let Some(s) = &stats {
        eprintln!(
            "  cache: {} entries, {} content blobs, {} total hits",
            s.entries, s.content_blobs, s.total_hits
        );
    }

    eprintln!("cortex MCP server ready ({} units, {} patterns, {} anti-patterns)",
        units.len(),
        store.all_patterns().map(|p| p.len()).unwrap_or(0),
        store.all_anti_patterns().map(|p| p.len()).unwrap_or(0),
    );

    // Session ID: for now we use a single implicit session per server process.
    // A future extension could derive this from an MCP session header if VS Code
    // starts sending one.
    let session_id = format!("session_{}", std::process::id());

    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() { continue; }

        let req: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => { eprintln!("warn: bad request: {e}"); continue; }
        };

        if req.get("id").is_none() { continue; } // notification

        let id = req["id"].clone();
        let method = req["method"].as_str().unwrap_or("");
        let params = req.get("params").cloned().unwrap_or(Value::Null);

        let result = match method {
            "initialize"  => Ok(initialize_result(&engine_name)),
            "tools/list"  => Ok(tools_list()),
            "tools/call"  => {
                let tool = params["name"].as_str().unwrap_or("");
                let args = params.get("arguments").cloned().unwrap_or(json!({}));
                let args_str = args.to_string();

                // Log the call regardless of cache hit.
                let _ = store.log_mcp_call(tool, &args_str);

                // Check response cache (skip for volatile tools).
                let cached = if UNCACHEABLE.contains(&tool) {
                    None
                } else {
                    get_cached_response(store.conn(), tool, &args_str, &index_version)
                        .unwrap_or(None)
                };

                if let Some(cached_text) = cached {
                    eprintln!("  [cache hit] {}", tool);
                    Ok(json!({
                        "content": [{ "type": "text", "text": cached_text }]
                    }))
                } else {
                    let result = tools::dispatch(
                        tool,
                        &args,
                        &store,
                        &units,
                        &sessions,
                        &session_id,
                        &repo_root,
                        &prefs_summary,
                    );

                    // Cache the result if it was successful and tool is cacheable.
                    if let Ok(ref res) = result {
                        if !UNCACHEABLE.contains(&tool) {
                            if let Some(text) = res["content"][0]["text"].as_str() {
                                let _ = cache_response(
                                    store.conn(), tool, &args_str,
                                    &index_version, text, CACHE_MAX_ENTRIES,
                                );
                            }
                        }
                    }

                    result
                }
            }
            other => Err(format!("unknown method: {other}")),
        };

        let response = match result {
            Ok(r)    => json!({ "jsonrpc": "2.0", "id": id, "result": r }),
            Err(msg) => json!({ "jsonrpc": "2.0", "id": id,
                "error": { "code": -32601, "message": msg } }),
        };

        writeln!(out, "{}", serde_json::to_string(&response)?)?;
        out.flush()?;
    }

    sessions.clear_session(&session_id);
    Ok(())
}

fn initialize_result(engine_name: &str) -> Value {
    json!({
        "protocolVersion": "2024-11-05",
        "capabilities": { "tools": {} },
        "serverInfo": {
            "name": "cortex",
            "version": env!("CARGO_PKG_VERSION"),
            "description": format!("{engine_name} semantic memory layer")
        }
    })
}

fn tools_list() -> Value {
    json!({
        "tools": [
            {
                "name": "semantic_search",
                "description": "Search the codebase by intent or concept. \
                                Returns the most semantically relevant API items. \
                                Use this FIRST before writing any engine code.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "query": { "type": "string", "description": "Intent or concept to search for." },
                        "limit": { "type": "integer", "description": "Max results (default: 5)." }
                    },
                    "required": ["query"]
                }
            },
            {
                "name": "get_item",
                "description": "Get full compressed details on a named API item — \
                                signature, fields, variants, methods.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "name": { "type": "string", "description": "Exact item name." }
                    },
                    "required": ["name"]
                }
            },
            {
                "name": "get_context",
                "description": "Get a pre-compiled, token-efficient context packet \
                                for the current task. Pass open file paths or a task \
                                description. Returns relevant API, patterns, anti-patterns, \
                                and notes in minimal form.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "hint": { "type": "string", "description": "Task description or open file paths." },
                        "token_budget": { "type": "integer", "description": "Max tokens to use (default: 2000)." },
                        "delta_include": { "type": "string", "description": "Optional substring include filter for changed paths." },
                        "delta_exclude": { "type": "string", "description": "Optional substring exclude filter for changed paths." },
                        "delta_max_files": { "type": "integer", "description": "Max changed files in context delta section (default: 8)." },
                        "delta_max_patch_lines": { "type": "integer", "description": "Max patch lines captured per changed file (default: 40)." }
                    },
                    "required": ["hint"]
                }
            },
            {
                "name": "get_delta",
                "description": "Get compressed git delta entries for working tree or from a commit range.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "since": { "type": "string", "description": "Optional start ref. Uses HEAD working tree diff when omitted." },
                        "include": { "type": "string", "description": "Optional substring include filter for changed paths." },
                        "exclude": { "type": "string", "description": "Optional substring exclude filter for changed paths." },
                        "max_files": { "type": "integer", "description": "Max changed files to return (default: 128)." },
                        "max_patch_lines": { "type": "integer", "description": "Max patch lines inspected per changed file (default: 40)." }
                    }
                }
            },
            {
                "name": "query_graph",
                "description": "Query the knowledge graph around an indexed item by name.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "name": { "type": "string", "description": "Indexed item name." },
                        "depth": { "type": "integer", "description": "Neighbor depth (default: 1)." }
                    },
                    "required": ["name"]
                }
            },
            {
                "name": "get_preferences",
                "description": "Return the active Copilot coding preferences summary loaded by cortex.",
                "inputSchema": { "type": "object", "properties": {} }
            },
            {
                "name": "recurrent_think",
                "description": "Iterative hypothesis refinement loop for complex tasks. \
                                Propose → Critique → Refine → Assess → Halt or Continue. \
                                Max 6 loops by default. Use for deep design decisions or multi-step problems.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "task": { "type": "string", "description": "Problem or feature description." },
                        "hypothesis": { "type": "string", "description": "Optional: current hypothesis to critique." },
                        "loop": { "type": "integer", "description": "Current loop index (default: 0)." },
                        "max_loops": { "type": "integer", "description": "Max iterations (default: 6, capped at 16)." },
                        "depth_mode": {
                            "type": "string",
                            "description": "Iteration preset: auto (default), shallow (2), deep (up to 16).",
                            "enum": ["auto", "shallow", "deep"]
                        }
                    },
                    "required": ["task"]
                }
            },
            {
                "name": "simulate_change",
                "description": "Dry-run impact predictor. Simulates what breaks if you change an item. \
                                Returns risk level (Low/Medium/High) and list of affected downstream items. \
                                Call before modifying widely-used types.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "item": { "type": "string", "description": "Name of the item to change." },
                        "change": { "type": "string", "description": "Description of the change (e.g., 'add new variant')." },
                        "depth": { "type": "integer", "description": "Transitive depth (default: 1)." }
                    },
                    "required": ["item"]
                }
            },
            {
                "name": "recall",
                "description": "Retrieve everything cortex knows about a topic — \
                                matching patterns, anti-patterns, annotations, and API items. \
                                Use when you need to know if we've solved this before.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "topic": { "type": "string", "description": "Topic, name, or concept to recall." }
                    },
                    "required": ["topic"]
                }
            },
            {
                "name": "list_patterns",
                "description": "List all approved code patterns with their intents. \
                                Includes use/revert/survival metrics and flags patterns below 40% survival. \
                                Check this before implementing any non-trivial logic.",
                "inputSchema": { "type": "object", "properties": {} }
            },
            {
                "name": "get_anti_patterns",
                "description": "Get all known anti-patterns — things Copilot must NOT do. \
                                Always check this before generating code.",
                "inputSchema": { "type": "object", "properties": {} }
            },
            {
                "name": "suggest_pattern",
                "description": "Suggest a pattern for Syn's review. Does NOT save it — \
                                queues it as a pending observation for manual approval only.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "name":   { "type": "string" },
                        "intent": { "type": "string" },
                        "body":   { "type": "string", "description": "The code pattern." },
                        "uses":   { "type": "array", "items": { "type": "string" }, "description": "API item names used." }
                    },
                    "required": ["name", "intent", "body"]
                }
            },
            {
                "name": "list_all",
                "description": "List all indexed API items, optionally filtered by kind.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "kind": {
                            "type": "string",
                            "description": "Filter: struct, enum, trait, fn. Omit for all.",
                            "enum": ["struct", "enum", "trait", "fn"]
                        }
                    }
                }
            }
        ]
    })
}
