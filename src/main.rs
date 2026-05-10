mod cache;
mod compressor;
mod crystallizer;
mod git;
mod graph;
mod memory;
mod mcp;
mod model;
mod planner;
mod prefs;
mod reasoner;
mod search;
mod watcher;

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

use memory::Store;

// ── CLI ───────────────────────────────────────────────────────────────────────

#[derive(Parser, Debug)]
#[command(name = "cortex", version, about = "Persistent semantic memory layer for Copilot")]
struct Cli {
    /// Path to the cortex database. Defaults to .cortex/memory.db in the project root.
    #[arg(long, global = true)]
    db: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Index a source directory (and optional quartz-ctx api-graph.json).
    Index(IndexArgs),

    /// Run the MCP skill server — Copilot calls cortex as a live tool.
    Serve(ServeArgs),

    /// Watch for file changes and queue them for review. Never auto-approves.
    Watch(WatchArgs),

    /// List pending observations queued by `watch` or Copilot.
    Review,

    /// Promote a pending observation to an approved pattern.
    Crystallize(CrystallizeArgs),

    /// Discard a pending observation.
    Dismiss(DismissArgs),

    /// Get a pre-compiled context packet for a task or set of files.
    Context(ContextArgs),

    /// Graph relation management and querying.
    #[command(subcommand)]
    Graph(GraphCmd),

    /// Preference file management.
    #[command(subcommand)]
    Prefs(PrefsCmd),

    /// Pattern management.
    #[command(subcommand)]
    Pattern(PatternCmd),

    /// Anti-pattern management.
    #[command(subcommand)]
    AntiPattern(AntiPatternCmd),

    /// Annotation management.
    #[command(subcommand)]
    Annotate(AnnotateCmd),

    /// Prune call log and run VACUUM to reclaim space.
    Prune {
        /// Number of MCP call log entries to keep.
        #[arg(long, default_value = "500")]
        keep_calls: usize,
    },

    /// Show memory store statistics.
    Status {
        #[arg(long)]
        full: bool,
    },
}

// ── Subcommand args ───────────────────────────────────────────────────────────

#[derive(Parser, Debug)]
struct IndexArgs {
    /// Source directory to parse and compress.
    #[arg(short, long, default_value = "src")]
    source: PathBuf,

    /// Optional path to a quartz-ctx api-graph.json to ingest alongside source.
    #[arg(long)]
    api_graph: Option<PathBuf>,

    /// Engine/project name label.
    #[arg(short, long, default_value = "Quartz")]
    name: String,
}

#[derive(Parser, Debug)]
struct ServeArgs {
    #[arg(short, long, default_value = "src")]
    source: PathBuf,

    #[arg(long, default_value = ".")]
    repo: PathBuf,

    #[arg(long)]
    api_graph: Option<PathBuf>,

    #[arg(long)]
    prefs: Option<PathBuf>,

    #[arg(short, long, default_value = "Quartz")]
    name: String,
}

#[derive(Parser, Debug)]
struct WatchArgs {
    #[arg(short, long, default_value = "src")]
    source: PathBuf,
}

#[derive(Parser, Debug)]
struct CrystallizeArgs {
    /// ID of the pending observation to promote.
    pub id: i64,
    #[arg(long)]
    pub name: String,
    #[arg(long)]
    pub intent: String,
    /// Override the observation body with custom code. Defaults to the observation's diff_hint.
    #[arg(long)]
    pub body: Option<String>,
    /// API item names this pattern uses.
    #[arg(long, value_delimiter = ',')]
    pub uses: Vec<String>,
    #[arg(long, value_delimiter = ',')]
    pub tags: Vec<String>,
}

#[derive(Parser, Debug)]
struct DismissArgs {
    pub id: i64,
}

#[derive(Parser, Debug)]
struct ContextArgs {
    /// Task description or space-separated file paths.
    pub hint: String,
    #[arg(long, default_value = "2000")]
    pub token_budget: usize,

    #[arg(long, default_value = ".")]
    pub repo: PathBuf,

    #[arg(long)]
    pub prefs: Option<PathBuf>,
}

#[derive(Subcommand, Debug)]
enum GraphCmd {
    Sync,
    AddPair {
        from: String,
        to: String,
    },
    AddConflict {
        from: String,
        to: String,
    },
    Query {
        name: String,
        #[arg(long, default_value = "1")]
        depth: u8,
    },
}

#[derive(Subcommand, Debug)]
enum PrefsCmd {
    Show {
        #[arg(long)]
        path: Option<PathBuf>,
    },
    Edit {
        #[arg(long)]
        path: Option<PathBuf>,
    },
}

#[derive(Subcommand, Debug)]
enum PatternCmd {
    /// List all approved patterns.
    List,
    /// Add a pattern directly.
    Add {
        #[arg(long)] name: String,
        #[arg(long)] intent: String,
        #[arg(long)] body: String,
        #[arg(long, value_delimiter = ',')] uses: Vec<String>,
        #[arg(long, value_delimiter = ',')] tags: Vec<String>,
    },
    /// Remove a pattern by id.
    Remove { id: i64 },
    /// Mark a pattern as reverted once and update survival rate.
    Revert { id: i64 },
    /// Show pattern survival health.
    Health,
}

#[derive(Subcommand, Debug)]
enum AntiPatternCmd {
    List,
    Add {
        #[arg(long)] description: String,
        #[arg(long)] wrong: String,
        #[arg(long)] correct: String,
        #[arg(long, value_delimiter = ',')] tags: Vec<String>,
    },
    Remove { id: i64 },
}

#[derive(Subcommand, Debug)]
enum AnnotateCmd {
    List,
    Add {
        #[arg(long)] topic: String,
        #[arg(long)] body: String,
        #[arg(long, value_delimiter = ',')] tags: Vec<String>,
    },
    Remove { id: i64 },
}

// ── Entry point ───────────────────────────────────────────────────────────────

fn main() -> Result<()> {
    let cli = Cli::parse();

    let db_path = cli.db.unwrap_or_else(|| PathBuf::from(".cortex/memory.db"));

    match cli.command {
        Command::Index(args)       => run_index(args, &db_path),
        Command::Serve(args)       => run_serve(args, &db_path),
        Command::Watch(args)       => run_watch(args, &db_path),
        Command::Review            => run_review(&db_path),
        Command::Crystallize(args) => run_crystallize(args, &db_path),
        Command::Dismiss(args)     => run_dismiss(args, &db_path),
        Command::Context(args)     => run_context(args, &db_path),
        Command::Graph(cmd)        => run_graph(cmd, &db_path),
        Command::Prefs(cmd)        => run_prefs(cmd),
        Command::Pattern(cmd)      => run_pattern(cmd, &db_path),
        Command::AntiPattern(cmd)  => run_anti_pattern(cmd, &db_path),
        Command::Annotate(cmd)     => run_annotate(cmd, &db_path),
        Command::Prune { keep_calls } => run_prune(keep_calls, &db_path),
        Command::Status { full }   => run_status(&db_path, full),
    }
}

// ── Command handlers ──────────────────────────────────────────────────────────

fn run_index(args: IndexArgs, db_path: &Path) -> Result<()> {
    let store = Store::open(db_path)?;

    eprintln!("cortex index: compressing {}", args.source.display());
    let mut units = compressor::compress_dir(&args.source)?;
    eprintln!("  source: {} items compressed", units.len());

    if let Some(graph_path) = &args.api_graph {
        let json = std::fs::read_to_string(graph_path)
            .with_context(|| format!("could not read api-graph: {}", graph_path.display()))?;
        let graph_items: Vec<model::ApiGraphItem> = serde_json::from_str(&json)?;
        let graph_units = compressor::compress_api_graph(&graph_items);
        eprintln!("  api-graph: {} items ingested from {}", graph_units.len(), graph_path.display());
        // Merge: api-graph items take precedence (they have richer doc)
        let source_ids: std::collections::HashSet<&str> = graph_units.iter().map(|u| u.id.as_str()).collect();
        units.retain(|u| !source_ids.contains(u.id.as_str()));
        units.extend(graph_units);
    }

    for unit in &units {
        store.upsert_unit(unit)?;
    }

    let synced = graph::sync_nodes(store.conn())?;
    let inferred = graph::infer_edges(store.conn(), &units)?;

    eprintln!("  total: {} units in index", units.len());
    eprintln!("  graph: {} nodes synced, {} edges inferred", synced, inferred);
    eprintln!("  db: {}", db_path.display());
    eprintln!("\ndone.");
    Ok(())
}

fn run_serve(args: ServeArgs, db_path: &Path) -> Result<()> {
    let store = Store::open(db_path)?;

    // Load units: prefer DB (already indexed), fall back to live parse
    let units = if store.unit_count()? > 0 {
        eprintln!("cortex serve: loading {} units from index", store.unit_count()?);
        store.all_units()?
    } else {
        eprintln!("cortex serve: index empty, compressing {} live", args.source.display());
        let mut units = compressor::compress_dir(&args.source)?;
        if let Some(graph_path) = &args.api_graph {
            let json = std::fs::read_to_string(graph_path)?;
            let graph_items: Vec<model::ApiGraphItem> = serde_json::from_str(&json)?;
            units.extend(compressor::compress_api_graph(&graph_items));
        }
        units
    };

    let prefs_path = args.prefs.unwrap_or_else(default_prefs_path);
    let prefs = prefs::load(&prefs_path).unwrap_or_default();
    let prefs_summary = prefs::render_for_copilot(&prefs);

    eprintln!("  {} units loaded — listening on stdio", units.len());
    mcp::serve(store, units, &args.name, args.repo, prefs_summary)
}

fn run_watch(args: WatchArgs, db_path: &Path) -> Result<()> {
    watcher::watch(&args.source, db_path)
}

fn run_review(db_path: &Path) -> Result<()> {
    let store = Store::open(db_path)?;
    crystallizer::list_pending(&store)
}

fn run_crystallize(args: CrystallizeArgs, db_path: &Path) -> Result<()> {
    let store = Store::open(db_path)?;
    crystallizer::crystallize_observation(
        &store,
        args.id,
        &args.name,
        &args.intent,
        args.body.as_deref(),
        args.uses,
        args.tags,
    )
}

fn run_dismiss(args: DismissArgs, db_path: &Path) -> Result<()> {
    let store = Store::open(db_path)?;
    crystallizer::dismiss_observation(&store, args.id)
}

fn run_context(args: ContextArgs, db_path: &Path) -> Result<()> {
    let store = Store::open(db_path)?;
    let packet = planner::build_context_packet(&store, &args.hint, args.token_budget, Some(&args.repo))?;

    let mut output = String::new();
    let prefs_path = args.prefs.unwrap_or_else(default_prefs_path);
    let prefs = prefs::load(&prefs_path).unwrap_or_default();
    let prefs_summary = prefs::render_for_copilot(&prefs);
    if !prefs_summary.trim().is_empty() {
        output.push_str(&prefs_summary);
        output.push('\n');
    }

    output.push_str(&planner::render_packet(&packet));
    print!("{}", output);
    eprintln!("\n[~{} tokens estimated]", packet.estimated_tokens);
    Ok(())
}

fn run_graph(cmd: GraphCmd, db_path: &Path) -> Result<()> {
    let store = Store::open(db_path)?;
    match cmd {
        GraphCmd::Sync => {
            let units = store.all_units()?;
            let synced = graph::sync_nodes(store.conn())?;
            let inferred = graph::infer_edges(store.conn(), &units)?;
            println!("graph synced: {} nodes, {} inferred edges", synced, inferred);
        }
        GraphCmd::AddPair { from, to } => {
            graph::add_edge(store.conn(), &from, &to, model::RelationType::Pairs)?;
            println!("added pair edge: {} -> {}", from, to);
        }
        GraphCmd::AddConflict { from, to } => {
            graph::add_edge(store.conn(), &from, &to, model::RelationType::Conflicts)?;
            println!("added conflict edge: {} -> {}", from, to);
        }
        GraphCmd::Query { name, depth } => {
            let unit = store.get_unit(&name)?;
            if let Some(u) = unit {
                let (edges, nodes) = graph::subgraph(store.conn(), &u.id, depth)?;
                println!("subgraph root: {}", u.name);
                println!("nodes: {}", nodes.len());
                println!("edges: {}", edges.len());
                for e in edges {
                    println!("{} -[{}]-> {}", e.from_id, e.relation.as_str(), e.to_id);
                }
            } else {
                println!("no unit found for {}", name);
            }
        }
    }
    Ok(())
}

fn run_prefs(cmd: PrefsCmd) -> Result<()> {
    match cmd {
        PrefsCmd::Show { path } => {
            let p = path.unwrap_or_else(default_prefs_path);
            let prefs = prefs::load(&p).unwrap_or_default();
            println!("{}", prefs::render_for_copilot(&prefs));
        }
        PrefsCmd::Edit { path } => {
            let p = path.unwrap_or_else(default_prefs_path);
            if !p.exists() {
                prefs::save(&prefs::Preferences::default(), &p)?;
            }
            let editor = std::env::var("EDITOR").unwrap_or_else(|_| {
                if cfg!(windows) { "notepad".to_string() } else { "vi".to_string() }
            });
            std::process::Command::new(editor).arg(&p).status()?;
        }
    }
    Ok(())
}

fn default_prefs_path() -> PathBuf {
    PathBuf::from(".cortex/prefs.toml")
}

fn run_pattern(cmd: PatternCmd, db_path: &Path) -> Result<()> {
    let store = Store::open(db_path)?;
    match cmd {
        PatternCmd::List => crystallizer::list_patterns(&store),
        PatternCmd::Add { name, intent, body, uses, tags } =>
            crystallizer::add_pattern(&store, &name, &intent, &body, uses, tags),
        PatternCmd::Remove { id } => crystallizer::remove_pattern(&store, id),
        PatternCmd::Revert { id } => crystallizer::report_revert(&store, id),
        PatternCmd::Health => crystallizer::list_pattern_health(&store),
    }
}

fn run_anti_pattern(cmd: AntiPatternCmd, db_path: &Path) -> Result<()> {
    let store = Store::open(db_path)?;
    match cmd {
        AntiPatternCmd::List => crystallizer::list_anti_patterns(&store),
        AntiPatternCmd::Add { description, wrong, correct, tags } =>
            crystallizer::add_anti_pattern(&store, &description, &wrong, &correct, tags),
        AntiPatternCmd::Remove { id } => crystallizer::remove_anti_pattern(&store, id),
    }
}

fn run_annotate(cmd: AnnotateCmd, db_path: &Path) -> Result<()> {
    let store = Store::open(db_path)?;
    match cmd {
        AnnotateCmd::List => crystallizer::list_annotations(&store),
        AnnotateCmd::Add { topic, body, tags } =>
            crystallizer::add_annotation(&store, &topic, &body, tags),
        AnnotateCmd::Remove { id } => crystallizer::remove_annotation(&store, id),
    }
}

fn run_status(db_path: &Path, full: bool) -> Result<()> {
    let store = Store::open(db_path)?;

    let report = build_status_report(&store, db_path, full)?;
    print!("{}", report);
    Ok(())
}

fn build_status_report(store: &Store, db_path: &Path, full: bool) -> Result<String> {

    let unit_count = store.unit_count()?;
    let patterns = store.all_patterns()?;
    let anti_patterns = store.all_anti_patterns()?;
    let annotations = store.all_annotations()?;
    let observations = store.all_observations()?;
    let hot = store.hot_tools(5)?;
    let cache = cache::cache_stats(store.conn()).ok();

    // Rough DB file size
    let db_size = std::fs::metadata(db_path)
        .map(|m| format_bytes(m.len()))
        .unwrap_or_else(|_| "unknown".to_string());

    let mut out = String::new();
    out.push_str("cortex status\n\n");
    out.push_str(&format!("  db:               {}\n", db_path.display()));
    out.push_str(&format!("  db size:          {}\n", db_size));
    out.push_str(&format!("  indexed units:    {}\n", unit_count));
    out.push_str(&format!("  patterns:         {}\n", patterns.len()));
    out.push_str(&format!("  anti-patterns:    {}\n", anti_patterns.len()));
    out.push_str(&format!("  annotations:      {}\n", annotations.len()));
    out.push_str(&format!("  pending review:   {}\n", observations.len()));

    if let Some(c) = cache {
        out.push('\n');
        out.push_str(&format!(
            "  response cache:   {} entries ({} cache hits total)\n",
            c.entries, c.total_hits
        ));
        out.push_str(&format!(
            "  content store:    {} blobs (~{} compressed)\n",
            c.content_blobs,
            format_bytes(c.approx_bytes as u64)
        ));
    }

    if !hot.is_empty() {
        out.push_str("\n  most-called tools:\n");
        for (tool, count) in &hot {
            out.push_str(&format!("    {:25} {}x\n", tool, count));
        }
    }

    if !observations.is_empty() {
        out.push_str(&format!(
            "\n  {} observation(s) waiting — run `cortex review`\n",
            observations.len()
        ));
    }

    if full {
        let (nodes, edges, inferred, manual) = store.graph_counts()?;
        let scratchpads = store.scratchpad_count()?;
        let recent_hot = store.hot_tools_recent(500, 5)?;
        let health = store.pattern_health_rows()?;

        out.push_str("\nfull details\n\n");
        out.push_str("  graph:\n");
        out.push_str(&format!("    nodes:           {}\n", nodes));
        out.push_str(&format!(
            "    edges:           {}  ({} inferred, {} manual)\n",
            edges, inferred, manual
        ));
        out.push_str(&format!("\n  scratchpads:       {} active\n", scratchpads));

        if !recent_hot.is_empty() {
            out.push_str("\n  top tools (last 500 calls):\n");
            for (tool, count) in &recent_hot {
                out.push_str(&format!("    {:20} {}x\n", tool, count));
            }
        }

        if !health.is_empty() {
            let low_count = health.iter().filter(|(_, _, _, _, s)| *s < 0.4).count();

            out.push_str("\n  pattern health:\n");
            for (_id, name, _uses, _reverted, survival) in &health {
                let (marker, tier) = if *survival < 0.4 {
                    ("⚠", "critical")
                } else if *survival < 0.8 {
                    ("!", "watch")
                } else {
                    ("✓", "healthy")
                };
                out.push_str(&format!(
                    "    {} {} ({:.0}%) [{}]\n",
                    marker,
                    name,
                    survival * 100.0,
                    tier
                ));
            }

            if low_count > 0 {
                out.push_str(&format!(
                    "\n  {} pattern(s) below 40% survival — run `cortex pattern health` and revise risky patterns.\n",
                    low_count
                ));
            }
        }
    }

    Ok(out)
}

fn run_prune(keep_calls: usize, db_path: &Path) -> Result<()> {
    let store = Store::open(db_path)?;

    let pruned_calls = cache::prune_call_log(store.conn(), keep_calls)?;
    println!("  pruned {} call log entries (keeping {})", pruned_calls, keep_calls);

    cache::vacuum(store.conn())?;
    println!("  vacuumed db");

    let db_size = std::fs::metadata(db_path)
        .map(|m| format_bytes(m.len()))
        .unwrap_or_else(|_| "unknown".to_string());
    println!("  db size now: {}", db_size);

    Ok(())
}

fn format_bytes(b: u64) -> String {
    if b < 1024 { format!("{}B", b) }
    else if b < 1024 * 1024 { format!("{:.1}KB", b as f64 / 1024.0) }
    else { format!("{:.2}MB", b as f64 / (1024.0 * 1024.0)) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_db_path(name: &str) -> PathBuf {
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_millis();
        std::env::temp_dir().join(format!("{}_{}.db", name, ts))
    }

    #[test]
    fn phase4_pattern_revert_reflects_in_status_full() {
        let db_path = temp_db_path("cortex_phase4_status_test");
        let store = Store::open(&db_path).expect("open store");

        crate::crystallizer::add_pattern(
            &store,
            "Grounded sound",
            "Play landing sound once",
            "if grounded_transition { Action::PlaySound(..) }",
            vec!["Action".to_string(), "Condition".to_string()],
            vec!["audio".to_string()],
        )
        .expect("add pattern");

        crate::crystallizer::report_revert(&store, 1).expect("revert pattern");

        let report = build_status_report(&store, &db_path, true).expect("status report");
        assert!(report.contains("pattern health:"));
        assert!(report.contains("Grounded sound"));
        assert!(report.contains("0%"));

        let _ = std::fs::remove_file(&db_path);
    }
}
