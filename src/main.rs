mod adr;
mod cache;
mod compressor;
mod consolidator;
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
use clap::{Parser, Subcommand, ValueEnum};
use serde::Serialize;
use serde_json::{json, Value};

use memory::Store;

// ── CLI ───────────────────────────────────────────────────────────────────────

#[derive(Parser, Debug)]
#[command(name = "cortex", version, about = "Persistent semantic memory layer for Copilot")]
struct Cli {
    /// Path to the cortex database. Defaults to .cortex/memory.db in the project root.
    #[arg(long, global = true)]
    db: Option<PathBuf>,

    /// Output format for script-safe automation.
    #[arg(long, global = true, value_enum, default_value_t = OutputFormat::Text)]
    format: OutputFormat,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// First-time project bootstrap: create launcher and MCP config files.
    Bootstrap(BootstrapArgs),

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

    /// Run production-style workflow health checks.
    #[command(subcommand)]
    Doctor(DoctorCmd),

    /// Search patterns, anti-patterns, annotations, and indexed units for a topic.
    Recall {
        /// Topic keyword to search for across all memory.
        topic: String,
    },

    /// Scan recent git diff for pattern relevance — shows which patterns apply to changed files.
    GitReview {
        /// Compare against this base ref (default: HEAD~1).
        #[arg(long, default_value = "HEAD~1")]
        base: String,

        /// Path to the repo root (default: current dir).
        #[arg(long)]
        repo: Option<PathBuf>,
    },

    /// Architecture Decision Records (ADRs).
    #[command(subcommand)]
    Adr(AdrCmd),

    /// Find and optionally merge duplicate/overlapping patterns using cosine similarity.
    Consolidate {
        /// Similarity threshold 0.0–1.0 (default 0.72). Pairs above this score are flagged.
        #[arg(long, default_value_t = 0.72)]
        threshold: f32,

        /// Just report candidates; do not merge anything.
        #[arg(long)]
        report: bool,
    },

    /// Log a self-correction: what was attempted, why it failed, and what the fix was.
    Correction {
        /// The thing that was attempted (wrong approach or snippet).
        #[arg(long)]
        attempted: String,

        /// Reason it failed.
        #[arg(long)]
        reason: String,

        /// The correct approach or fix.
        #[arg(long)]
        fix: String,

        /// Comma-separated tags.
        #[arg(long, default_value = "")]
        tags: String,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum OutputFormat {
    Text,
    Json,
}

// ── ADR subcommand ────────────────────────────────────────────────────────────

#[derive(Subcommand, Debug)]
enum AdrCmd {
    /// Record a new Architecture Decision.
    New {
        #[arg(long)] title: String,
        #[arg(long)] context: String,
        #[arg(long)] decision: String,
        #[arg(long, default_value = "")] reasoning: String,
        #[arg(long, default_value = "")] alternatives: String,
        #[arg(long, default_value = "")] consequences: String,
        /// Comma-separated concept tags for context matching.
        #[arg(long, default_value = "")] tags: String,
    },
    /// List all ADRs.
    List,
    /// Show a single ADR by number.
    Show {
        #[arg()] number: i64,
    },
    /// Deprecate or supersede an ADR.
    Deprecate {
        #[arg()] number: i64,
        #[arg(long)] superseded_by: Option<i64>,
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

    /// Optional scope prefix prepended to all unit IDs (e.g. "synful").
    /// Use when indexing multiple source roots into the same DB to avoid ID collisions.
    #[arg(long)]
    scope: Option<String>,
}

#[derive(Parser, Debug)]
struct BootstrapArgs {
    /// Workspace root where .cortex/ and .vscode/ should be created.
    #[arg(long, default_value = ".")]
    repo: PathBuf,

    /// Primary source path used by the MCP serve entry.
    #[arg(long, default_value = "src")]
    source: String,

    /// Project display name used by MCP serve.
    #[arg(long)]
    name: Option<String>,

    /// Overwrite existing files when present.
    #[arg(long, default_value_t = false)]
    force: bool,
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

    /// Optional include filter for delta paths in the context packet.
    #[arg(long)]
    pub delta_include: Option<String>,

    /// Optional exclude filter for delta paths in the context packet.
    #[arg(long)]
    pub delta_exclude: Option<String>,

    /// Max changed files to include in context deltas.
    #[arg(long, default_value = "8")]
    pub delta_max_files: usize,
}

#[derive(Subcommand, Debug)]
enum DoctorCmd {
    /// Full workflow smoke test for automation and CI checks.
    Workflow(DoctorWorkflowArgs),
}

#[derive(Parser, Debug)]
struct DoctorWorkflowArgs {
    #[arg(long, default_value = ".")]
    repo: PathBuf,

    #[arg(long, default_value = "src")]
    source: PathBuf,

    #[arg(long, default_value = "Quartz")]
    name: String,

    /// Mutate and rollback a sentinel pattern as part of validation.
    #[arg(long, default_value_t = false)]
    mutate_pattern: bool,

    /// Optional include filter for delta checks.
    #[arg(long)]
    delta_include: Option<String>,

    /// Optional exclude filter for delta checks.
    #[arg(long)]
    delta_exclude: Option<String>,

    /// Max changed files to inspect in delta checks.
    #[arg(long, default_value = "25")]
    delta_max_files: usize,
}

#[derive(Subcommand, Debug)]
enum GraphCmd {
    Sync,
    AddPair {
        from: String,
        to: String,
        /// Relation type: pairs (default), owns, uses, calls, implements, conflicts, derived_from
        #[arg(long, default_value = "pairs")]
        relation: String,
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
    let format = cli.format;

    match cli.command {
        Command::Bootstrap(args)   => run_bootstrap(args, &db_path),
        Command::Index(args)       => run_index(args, &db_path),
        Command::Serve(args)       => run_serve(args, &db_path),
        Command::Watch(args)       => run_watch(args, &db_path),
        Command::Review            => run_review(&db_path),
        Command::Crystallize(args) => run_crystallize(args, &db_path),
        Command::Dismiss(args)     => run_dismiss(args, &db_path),
        Command::Context(args)     => run_context(args, &db_path),
        Command::Graph(cmd)        => run_graph(cmd, &db_path),
        Command::Prefs(cmd)        => run_prefs(cmd),
        Command::Pattern(cmd)      => run_pattern(cmd, &db_path, format),
        Command::AntiPattern(cmd)  => run_anti_pattern(cmd, &db_path, format),
        Command::Annotate(cmd)     => run_annotate(cmd, &db_path, format),
        Command::Prune { keep_calls } => run_prune(keep_calls, &db_path),
        Command::Status { full }   => run_status(&db_path, full, format),
        Command::Doctor(cmd)       => run_doctor(cmd, &db_path, format),
        Command::Recall { topic }  => run_recall(&topic, &db_path, format),
        Command::GitReview { base, repo } => run_git_review(&base, repo.as_deref(), &db_path),
        Command::Adr(cmd)          => run_adr(cmd, &db_path),
        Command::Consolidate { threshold, report } => run_consolidate(threshold, report, &db_path),
        Command::Correction { attempted, reason, fix, tags } => {
            let tag_vec: Vec<String> = tags.split(',').map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty()).collect();
            run_correction(&attempted, &reason, &fix, &tag_vec, &db_path)
        }
    }
}

fn run_bootstrap(args: BootstrapArgs, db_path: &Path) -> Result<()> {
    let repo = args.repo;
    let cortex_dir = repo.join(".cortex");
    let vscode_dir = repo.join(".vscode");
    std::fs::create_dir_all(&cortex_dir)
        .with_context(|| format!("failed to create {}", cortex_dir.display()))?;
    std::fs::create_dir_all(&vscode_dir)
        .with_context(|| format!("failed to create {}", vscode_dir.display()))?;

    let project_name = args.name.unwrap_or_else(|| {
        repo.file_name()
            .map(|s| s.to_string_lossy().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "Project".to_string())
    });

    let script_path = cortex_dir.join("cortex.ps1");
    let index_path = cortex_dir.join("index-sources.json");
    let mcp_path = vscode_dir.join("mcp.json");

    if args.force || !script_path.exists() {
        std::fs::write(&script_path, bootstrap_cortex_ps1_template())
            .with_context(|| format!("failed to write {}", script_path.display()))?;
        println!("wrote {}", script_path.display());
    } else {
        println!("kept existing {}", script_path.display());
    }

    if args.force || !index_path.exists() {
        let index_json = json!({
            "targets": [
                {
                    "source": args.source,
                    "name": project_name,
                    "scope": Value::Null,
                }
            ]
        });
        std::fs::write(&index_path, serde_json::to_string_pretty(&index_json)?)
            .with_context(|| format!("failed to write {}", index_path.display()))?;
        println!("wrote {}", index_path.display());
    } else {
        println!("kept existing {}", index_path.display());
    }

    let mut mcp: Value = if mcp_path.exists() {
        let raw = std::fs::read_to_string(&mcp_path)
            .with_context(|| format!("failed to read {}", mcp_path.display()))?;
        serde_json::from_str(&raw).unwrap_or_else(|_| json!({ "servers": {}, "inputs": [] }))
    } else {
        json!({ "servers": {}, "inputs": [] })
    };

    if !mcp.is_object() {
        mcp = json!({ "servers": {}, "inputs": [] });
    }
    if !mcp.get("servers").map(|v| v.is_object()).unwrap_or(false) {
        mcp["servers"] = json!({});
    }
    if !mcp.get("inputs").map(|v| v.is_array()).unwrap_or(false) {
        mcp["inputs"] = json!([]);
    }

    mcp["servers"]["cortex"] = json!({
        "type": "stdio",
        "command": "cortex/target/debug/cortex.exe",
        "args": [
            "--db",
            db_path.to_string_lossy().replace('\\', "/"),
            "serve",
            "--source",
            "src",
            "--repo",
            ".",
            "--name",
            project_name
        ],
        "description": "Cortex MCP direct binary server. Reindex via .cortex/cortex.ps1 reindex (uses .cortex/index-sources.json)."
    });

    std::fs::write(&mcp_path, serde_json::to_string_pretty(&mcp)?)
        .with_context(|| format!("failed to write {}", mcp_path.display()))?;
    println!("updated {}", mcp_path.display());

    println!("\nbootstrap complete:");
    println!("  1. Build cortex binary: cargo build --manifest-path cortex/Cargo.toml");
    println!("  2. Index sources: .\\.cortex\\cortex.ps1 reindex");
    println!("  3. Start MCP server: .\\.cortex\\cortex.ps1 serve");
    Ok(())
}

fn bootstrap_cortex_ps1_template() -> &'static str {
    r#"# cortex.ps1 (bootstrap template)
param(
    [Parameter(Position=0)]
    [string]$Command = "serve",

    [Parameter(Position=1, ValueFromRemainingArguments=$true)]
    [string[]]$Rest
)

$DB = ".cortex\memory.db"
$INDEX_CONFIG = ".cortex\index-sources.json"
$BIN = "cortex\target\debug\cortex.exe"

function Ensure-Binary {
    if (-not (Test-Path $BIN)) {
        Write-Error "Cortex binary not found at $BIN. Run: cargo build --manifest-path cortex/Cargo.toml"
        exit 1
    }
}

function Get-PrimarySource {
    if (Test-Path $INDEX_CONFIG) {
        try {
            $cfg = Get-Content -Raw -Path $INDEX_CONFIG | ConvertFrom-Json
            if ($cfg.targets -and $cfg.targets.Count -gt 0 -and $cfg.targets[0].source) {
                return [string]$cfg.targets[0].source
            }
        } catch {}
    }
    return "src"
}

function Setup-Mcp {
    $path = ".vscode\mcp.json"
    $cfg = $null
    if (Test-Path $path) {
        try { $cfg = Get-Content -Raw -Path $path | ConvertFrom-Json } catch { $cfg = $null }
    }
    if (-not $cfg) { $cfg = [pscustomobject]@{} }
    if (-not $cfg.servers) { $cfg | Add-Member -NotePropertyName servers -NotePropertyValue ([pscustomobject]@{}) -Force }
    if (-not $cfg.inputs)  { $cfg | Add-Member -NotePropertyName inputs  -NotePropertyValue @() -Force }

    $cfg.servers | Add-Member -NotePropertyName cortex -NotePropertyValue ([pscustomobject]@{
        type = "stdio"
        command = "cortex/target/debug/cortex.exe"
        args = @("--db", ".cortex/memory.db", "serve", "--source", (Get-PrimarySource), "--repo", ".", "--name", "Project")
        description = "Cortex MCP direct binary server. Reindex via .cortex/cortex.ps1 reindex."
    }) -Force

    Set-Content -Path $path -Value ($cfg | ConvertTo-Json -Depth 20) -Encoding UTF8
    Write-Host "[cortex] updated $path"
}

Ensure-Binary

switch ($Command) {
    "serve" {
        $source = Get-PrimarySource
        & $BIN --db $DB serve --source $source --repo . --name Project
        exit $LASTEXITCODE
    }
    "reindex" {
        if (Test-Path $INDEX_CONFIG) {
            $cfg = Get-Content -Raw -Path $INDEX_CONFIG | ConvertFrom-Json
            foreach ($t in $cfg.targets) {
                if (-not $t.source) { continue }
                $name = if ($t.name) { [string]$t.name } else { "Project" }
                $args = @("--db", $DB, "index", "--source", [string]$t.source, "--name", $name)
                if ($t.scope) { $args += @("--scope", [string]$t.scope) }
                & $BIN @args
                if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
            }
        } else {
            & $BIN --db $DB index --source src --name Project
            exit $LASTEXITCODE
        }
        Write-Host "[cortex] reindex complete"
    }
    "setup-mcp" {
        Setup-Mcp
    }
    "status" {
        & $BIN --db $DB --format json status --full
        exit $LASTEXITCODE
    }
    "doctor" {
        $source = Get-PrimarySource
        & $BIN --db $DB --format json doctor workflow --repo . --source $source --name Project
        exit $LASTEXITCODE
    }
    default {
        & $BIN --db $DB $Command @Rest
        exit $LASTEXITCODE
    }
}
"#
}

// ── Command handlers ──────────────────────────────────────────────────────────

fn run_index(args: IndexArgs, db_path: &Path) -> Result<()> {
    let store = Store::open(db_path)?;

    eprintln!("cortex index: compressing {}", args.source.display());
    let (mut units, members) = compressor::compress_dir(&args.source, args.scope.as_deref())?;
    eprintln!("  source: {} items compressed, {} members", units.len(), members.len());

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
    for member in &members {
        store.upsert_member(member)?;
    }

    let synced = graph::sync_nodes(store.conn())?;
    let all_units = store.all_units()?;
    let inferred = graph::infer_edges(store.conn(), &all_units)?;

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
        let (mut units, _members) = compressor::compress_dir(&args.source, None)?;
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
    let delta_opts = git::DeltaOptions {
        include: args.delta_include,
        exclude: args.delta_exclude,
        max_files: args.delta_max_files,
        max_patch_lines: 40,
    };
    let packet = planner::build_context_packet(
        &store,
        &args.hint,
        args.token_budget,
        Some(&args.repo),
        Some(&delta_opts),
    )?;

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
        GraphCmd::AddPair { from, to, relation } => {
            let rel = model::RelationType::from_str(&relation)
                .unwrap_or(model::RelationType::Pairs);
            graph::add_edge(store.conn(), &from, &to, rel)?;
            println!("added {} edge: {} -> {}", rel.as_str(), from, to);
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

fn run_pattern(cmd: PatternCmd, db_path: &Path, format: OutputFormat) -> Result<()> {
    let store = Store::open(db_path)?;
    if format == OutputFormat::Text {
        return match cmd {
            PatternCmd::List => crystallizer::list_patterns(&store),
            PatternCmd::Add { name, intent, body, uses, tags } =>
                crystallizer::add_pattern(&store, &name, &intent, &body, uses, tags),
            PatternCmd::Remove { id } => crystallizer::remove_pattern(&store, id),
            PatternCmd::Revert { id } => crystallizer::report_revert(&store, id),
            PatternCmd::Health => crystallizer::list_pattern_health(&store),
        };
    }

    match cmd {
        PatternCmd::List => {
            let patterns = store.all_patterns()?;
            print_json(&patterns)
        }
        PatternCmd::Add { name, intent, body, uses, tags } => {
            let id = store.insert_pattern(&model::Pattern {
                id: None,
                name: name.clone(),
                intent: intent.clone(),
                body,
                uses,
                tags,
                approved_at: chrono::Utc::now(),
                use_count: 0,
                reverted_count: 0,
                survival_rate: 1.0,
            })?;
            print_json(&json!({"ok": true, "action": "add", "id": id, "name": name, "intent": intent}))
        }
        PatternCmd::Remove { id } => {
            store.delete_pattern(id)?;
            print_json(&json!({"ok": true, "action": "remove", "id": id}))
        }
        PatternCmd::Revert { id } => {
            store.pattern_reverted(id)?;
            print_json(&json!({"ok": true, "action": "revert", "id": id}))
        }
        PatternCmd::Health => {
            let rows = store.pattern_health_rows()?;
            let health: Vec<_> = rows
                .into_iter()
                .map(|(id, name, use_count, reverted_count, survival_rate)| {
                    json!({
                        "id": id,
                        "name": name,
                        "use_count": use_count,
                        "reverted_count": reverted_count,
                        "survival_rate": survival_rate
                    })
                })
                .collect();
            print_json(&json!({"ok": true, "action": "health", "patterns": health}))
        }
    }
}

fn run_anti_pattern(cmd: AntiPatternCmd, db_path: &Path, format: OutputFormat) -> Result<()> {
    let store = Store::open(db_path)?;
    if format == OutputFormat::Text {
        return match cmd {
            AntiPatternCmd::List => crystallizer::list_anti_patterns(&store),
            AntiPatternCmd::Add { description, wrong, correct, tags } =>
                crystallizer::add_anti_pattern(&store, &description, &wrong, &correct, tags),
            AntiPatternCmd::Remove { id } => crystallizer::remove_anti_pattern(&store, id),
        };
    }

    match cmd {
        AntiPatternCmd::List => {
            let anti_patterns = store.all_anti_patterns()?;
            print_json(&anti_patterns)
        }
        AntiPatternCmd::Add { description, wrong, correct, tags } => {
            let id = store.insert_anti_pattern(&model::AntiPattern {
                id: None,
                description: description.clone(),
                wrong,
                correct,
                tags,
                added_at: chrono::Utc::now(),
            })?;
            print_json(&json!({"ok": true, "action": "add", "id": id, "description": description}))
        }
        AntiPatternCmd::Remove { id } => {
            store.delete_anti_pattern(id)?;
            print_json(&json!({"ok": true, "action": "remove", "id": id}))
        }
    }
}

fn run_annotate(cmd: AnnotateCmd, db_path: &Path, format: OutputFormat) -> Result<()> {
    let store = Store::open(db_path)?;
    if format == OutputFormat::Text {
        return match cmd {
            AnnotateCmd::List => crystallizer::list_annotations(&store),
            AnnotateCmd::Add { topic, body, tags } =>
                crystallizer::add_annotation(&store, &topic, &body, tags),
            AnnotateCmd::Remove { id } => crystallizer::remove_annotation(&store, id),
        };
    }

    match cmd {
        AnnotateCmd::List => {
            let annotations = store.all_annotations()?;
            print_json(&annotations)
        }
        AnnotateCmd::Add { topic, body, tags } => {
            let id = store.insert_annotation(&model::Annotation {
                id: None,
                topic: topic.clone(),
                body,
                tags,
                added_at: chrono::Utc::now(),
            })?;
            print_json(&json!({"ok": true, "action": "add", "id": id, "topic": topic}))
        }
        AnnotateCmd::Remove { id } => {
            store.delete_annotation(id)?;
            print_json(&json!({"ok": true, "action": "remove", "id": id}))
        }
    }
}

fn run_status(db_path: &Path, full: bool, format: OutputFormat) -> Result<()> {
    let store = Store::open(db_path)?;

    if format == OutputFormat::Json {
        let report = build_status_json(&store, db_path, full)?;
        print_json(&report)?;
        return Ok(());
    }

    let report = build_status_report(&store, db_path, full)?;
    print!("{}", report);
    Ok(())
}

fn build_status_json(store: &Store, db_path: &Path, full: bool) -> Result<serde_json::Value> {
    let unit_count = store.unit_count()?;
    let patterns = store.all_patterns()?;
    let anti_patterns = store.all_anti_patterns()?;
    let annotations = store.all_annotations()?;
    let observations = store.all_observations()?;
    let hot = store.hot_tools(5)?;
    let cache = cache::cache_stats(store.conn()).ok();
    let db_size = std::fs::metadata(db_path).map(|m| m.len()).unwrap_or(0);

    let mut root = json!({
        "db": db_path.display().to_string(),
        "db_bytes": db_size,
        "indexed_units": unit_count,
        "patterns": patterns.len(),
        "anti_patterns": anti_patterns.len(),
        "annotations": annotations.len(),
        "pending_review": observations.len(),
        "hot_tools": hot,
    });

    if let Some(c) = cache {
        root["cache"] = json!({
            "entries": c.entries,
            "total_hits": c.total_hits,
            "content_blobs": c.content_blobs,
            "approx_bytes": c.approx_bytes,
        });
    }

    if full {
        let (nodes, edges, inferred, manual) = store.graph_counts()?;
        let scratchpads = store.scratchpad_count()?;
        let recent_hot = store.hot_tools_recent(500, 5)?;
        let health = store.pattern_health_rows()?;
        root["full"] = json!({
            "graph": {
                "nodes": nodes,
                "edges": edges,
                "inferred": inferred,
                "manual": manual,
            },
            "scratchpads": scratchpads,
            "recent_hot_tools": recent_hot,
            "pattern_health": health
        });
    }

    Ok(root)
}

fn run_doctor(cmd: DoctorCmd, db_path: &Path, format: OutputFormat) -> Result<()> {
    match cmd {
        DoctorCmd::Workflow(args) => run_doctor_workflow(args, db_path, format),
    }
}

#[derive(Serialize)]
struct DoctorCheck {
    step: String,
    pass: bool,
    detail: String,
}

fn run_doctor_workflow(args: DoctorWorkflowArgs, db_path: &Path, format: OutputFormat) -> Result<()> {
    let store = Store::open(db_path)?;
    let mut checks: Vec<DoctorCheck> = Vec::new();

    let unit_count = store.unit_count()?;
    checks.push(DoctorCheck {
        step: "index_present".to_string(),
        pass: unit_count > 0,
        detail: format!("indexed_units={}", unit_count),
    });

    let delta_opts = git::DeltaOptions {
        include: args.delta_include,
        exclude: args.delta_exclude,
        max_files: args.delta_max_files,
        max_patch_lines: 12,
    };
    let deltas = git::head_deltas_with_options(&args.repo, &delta_opts)?;
    checks.push(DoctorCheck {
        step: "delta_query".to_string(),
        pass: true,
        detail: format!("delta_files_returned={}", deltas.len()),
    });

    let packet = planner::build_context_packet(
        &store,
        "workflow doctor check",
        800,
        Some(&args.repo),
        Some(&delta_opts),
    )?;
    checks.push(DoctorCheck {
        step: "context_packet".to_string(),
        pass: true,
        detail: format!("estimated_tokens={} relevant_units={} deltas={}", packet.estimated_tokens, packet.relevant_units.len(), packet.deltas.len()),
    });

    let full_status = build_status_report(&store, db_path, true)?;
    checks.push(DoctorCheck {
        step: "status_full_render".to_string(),
        pass: full_status.contains("full details"),
        detail: "status --full report generated".to_string(),
    });

    if args.mutate_pattern {
        let pattern = model::Pattern {
            id: None,
            name: format!("doctor sentinel {}", chrono::Utc::now().timestamp()),
            intent: "Doctor mutation check".to_string(),
            body: "if grounded_transition { Action::PlaySound(..) }".to_string(),
            uses: vec!["Action".to_string(), "Condition".to_string()],
            tags: vec!["doctor".to_string(), "workflow".to_string()],
            approved_at: chrono::Utc::now(),
            use_count: 0,
            reverted_count: 0,
            survival_rate: 1.0,
        };

        let id = store.insert_pattern(&pattern)?;
        store.pattern_reverted(id)?;
        store.delete_pattern(id)?;
        checks.push(DoctorCheck {
            step: "pattern_roundtrip".to_string(),
            pass: true,
            detail: format!("added_reverted_removed_pattern_id={}", id),
        });
    }

    let pass = checks.iter().all(|c| c.pass);
    if format == OutputFormat::Json {
        print_json(&json!({
            "ok": pass,
            "checks": checks
        }))?;
    } else {
        println!("workflow doctor:\n");
        for c in &checks {
            let marker = if c.pass { "✓" } else { "✗" };
            println!("  {} {:22} {}", marker, c.step, c.detail);
        }
    }

    if !pass {
        anyhow::bail!("workflow doctor failed one or more checks");
    }
    Ok(())
}

fn print_json<T: Serialize>(v: &T) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(v)?);
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

fn run_recall(topic: &str, db_path: &Path, format: OutputFormat) -> Result<()> {
    let store = Store::open(db_path)?;
    let topic_lower = topic.to_lowercase();

    let units = store.all_units()?;
    let matched_units: Vec<_> = units
        .iter()
        .filter(|u| {
            u.name.to_lowercase().contains(&topic_lower)
                || u.compressed.to_lowercase().contains(&topic_lower)
        })
        .take(6)
        .collect();

    let patterns = store.all_patterns()?;
    let matched_patterns: Vec<_> = patterns
        .iter()
        .filter(|p| {
            p.name.to_lowercase().contains(&topic_lower)
                || p.intent.to_lowercase().contains(&topic_lower)
                || p.uses.iter().any(|u| u.to_lowercase().contains(&topic_lower))
                || p.tags.iter().any(|t| t.to_lowercase().contains(&topic_lower))
        })
        .collect();

    let aps = store.all_anti_patterns()?;
    let matched_aps: Vec<_> = aps
        .iter()
        .filter(|ap| {
            ap.description.to_lowercase().contains(&topic_lower)
                || ap.wrong.to_lowercase().contains(&topic_lower)
                || ap.tags.iter().any(|t| t.to_lowercase().contains(&topic_lower))
        })
        .collect();

    let annotations = store.all_annotations()?;
    let matched_annotations: Vec<_> = annotations
        .iter()
        .filter(|a| {
            a.topic.to_lowercase().contains(&topic_lower)
                || a.body.to_lowercase().contains(&topic_lower)
                || a.tags.iter().any(|t| t.to_lowercase().contains(&topic_lower))
        })
        .collect();

    if format == OutputFormat::Json {
        print_json(&json!({
            "topic": topic,
            "units": matched_units.iter().map(|u| json!({
                "id": u.id,
                "name": u.name,
                "kind": u.kind,
                "summary": u.compressed.chars().take(200).collect::<String>(),
            })).collect::<Vec<_>>(),
            "patterns": matched_patterns.iter().map(|p| json!({
                "id": p.id,
                "name": p.name,
                "intent": p.intent,
                "body": p.body,
                "survival_rate": p.survival_rate,
            })).collect::<Vec<_>>(),
            "anti_patterns": matched_aps.iter().map(|ap| json!({
                "id": ap.id,
                "description": ap.description,
                "wrong": ap.wrong,
                "correct": ap.correct,
            })).collect::<Vec<_>>(),
            "annotations": matched_annotations.iter().map(|a| json!({
                "id": a.id,
                "topic": a.topic,
                "body": a.body,
                "tags": a.tags,
            })).collect::<Vec<_>>(),
        }))?;
        return Ok(());
    }

    println!("# Recall: `{topic}`\n");

    if !matched_units.is_empty() {
        println!("## Indexed Units");
        for u in &matched_units {
            println!("### {} ({})", u.name, u.kind);
            let preview: String = u.compressed.chars().take(300).collect();
            println!("{}", preview);
            println!();
        }
    }

    if !matched_patterns.is_empty() {
        println!("## Patterns");
        for p in &matched_patterns {
            println!("### {} — {}", p.name, p.intent);
            println!("{}", p.body);
            println!("  survival: {:.0}%", p.survival_rate * 100.0);
            println!();
        }
    }

    if !matched_aps.is_empty() {
        println!("## Anti-Patterns");
        for ap in &matched_aps {
            println!("### {}", ap.description);
            println!("  WRONG:   {}", ap.wrong);
            println!("  CORRECT: {}", ap.correct);
            println!();
        }
    }

    if !matched_annotations.is_empty() {
        println!("## Annotations");
        for a in &matched_annotations {
            println!("### {}", a.topic);
            println!("{}", a.body);
            println!();
        }
    }

    let total = matched_units.len() + matched_patterns.len() + matched_aps.len() + matched_annotations.len();
    if total == 0 {
        println!("No results found for `{topic}`.");
    }

    Ok(())
}

fn run_git_review(base: &str, repo: Option<&Path>, db_path: &Path) -> Result<()> {
    let repo_root = repo.unwrap_or_else(|| Path::new("."));
    let store = Store::open(db_path)?;

    // Get changed files from git diff
    let output = std::process::Command::new("git")
        .args(["diff", "--name-only", base, "HEAD"])
        .current_dir(repo_root)
        .output()
        .context("git diff failed — is this a git repo?")?;

    let changed_files: Vec<String> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| l.to_string())
        .collect();

    if changed_files.is_empty() {
        println!("git-review: no changed files between {} and HEAD", base);
        return Ok(());
    }

    // Read content of changed .rs files to extract API names mentioned
    let mut mentioned_apis: Vec<String> = Vec::new();
    for f in &changed_files {
        if !f.ends_with(".rs") { continue; }
        let path = repo_root.join(f);
        if let Ok(src) = std::fs::read_to_string(&path) {
            // Collect capitalised identifiers (likely API types/enums)
            for word in src.split(|c: char| !c.is_alphanumeric() && c != '_') {
                if word.len() >= 4 && word.chars().next().map_or(false, |c| c.is_uppercase()) {
                    mentioned_apis.push(word.to_string());
                }
            }
        }
    }
    mentioned_apis.sort();
    mentioned_apis.dedup();

    // Match patterns whose `uses` overlap with mentioned APIs
    let patterns = store.all_patterns()?;
    let anti_patterns = store.all_anti_patterns()?;

    let relevant_patterns: Vec<_> = patterns.iter().filter(|p| {
        p.uses.iter().any(|u| mentioned_apis.iter().any(|m| m == u))
    }).collect();

    let relevant_aps: Vec<_> = anti_patterns.iter().filter(|ap| {
        ap.tags.iter().any(|t| mentioned_apis.iter().any(|m| m.to_lowercase().contains(&t.to_lowercase())))
        || mentioned_apis.iter().any(|m| ap.wrong.contains(m.as_str()) || ap.description.contains(m.as_str()))
    }).collect();

    println!("git-review: {} changed files ({}..HEAD)\n", changed_files.len(), base);
    println!("Changed files:");
    for f in &changed_files { println!("  {}", f); }
    println!();

    if relevant_patterns.is_empty() && relevant_aps.is_empty() {
        println!("No patterns or anti-patterns matched the changed files.");
        return Ok(());
    }

    if !relevant_patterns.is_empty() {
        println!("## Relevant Patterns ({} matched)\n", relevant_patterns.len());
        for p in &relevant_patterns {
            println!("  [{}] {} (survival {:.0}%)", p.id.unwrap_or(0), p.name, p.survival_rate * 100.0);
            println!("      {}", p.intent);
            println!("      uses: {}", p.uses.join(", "));
            println!();
        }
    }

    if !relevant_aps.is_empty() {
        println!("## Relevant Anti-Patterns ({} matched)\n", relevant_aps.len());
        for ap in &relevant_aps {
            println!("  [{}] {}", ap.id.unwrap_or(0), ap.description);
            println!("      WRONG:   {}", ap.wrong);
            println!("      CORRECT: {}", ap.correct);
            println!();
        }
    }

    println!("Run `cortex pattern revert <id>` to mark a pattern as not used in this diff.");
    Ok(())
}

// ── ADR handler ────────────────────────────────────────────────────────────────

fn run_adr(cmd: AdrCmd, db_path: &Path) -> Result<()> {
    let store = Store::open(db_path)?;
    match cmd {
        AdrCmd::New { title, context, decision, reasoning, alternatives, consequences, tags } => {
            let concept_tags: Vec<String> = tags.split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
            let number = store.next_adr_number()?;
            let a = model::Adr {
                id: None,
                adr_number: number,
                title: title.clone(),
                status: "accepted".into(),
                context,
                decision,
                reasoning,
                alternatives,
                consequences,
                concept_tags,
                superseded_by: None,
                created_at: chrono::Utc::now(),
                updated_at: chrono::Utc::now(),
            };
            let id = store.insert_adr(&a)?;
            println!("ADR-{:03}: {} (id={})", number, title, id);
        }
        AdrCmd::List => {
            let adrs = store.all_adrs()?;
            if adrs.is_empty() {
                println!("No ADRs recorded yet.");
            }
            for a in adrs {
                println!("ADR-{:03} [{}] {}", a.adr_number, a.status, a.title);
            }
        }
        AdrCmd::Show { number } => {
            match store.get_adr(number)? {
                None => println!("ADR-{:03} not found.", number),
                Some(a) => {
                    println!("{}", adr::format_for_context(&a));
                    println!("Reasoning: {}", a.reasoning);
                    if !a.alternatives.is_empty() {
                        println!("Alternatives considered: {}", a.alternatives);
                    }
                    if !a.consequences.is_empty() {
                        println!("Consequences: {}", a.consequences);
                    }
                    if !a.concept_tags.is_empty() {
                        println!("Tags: {}", a.concept_tags.join(", "));
                    }
                }
            }
        }
        AdrCmd::Deprecate { number, superseded_by } => {
            match store.get_adr(number)? {
                None => println!("ADR-{:03} not found.", number),
                Some(a) => {
                    let id = a.id.unwrap();
                    let status = if superseded_by.is_some() { "superseded" } else { "deprecated" };
                    store.update_adr_status(id, status, superseded_by)?;
                    println!("ADR-{:03} marked as {}.", number, status);
                }
            }
        }
    }
    Ok(())
}

// ── Consolidate handler ────────────────────────────────────────────────────────

fn run_consolidate(threshold: f32, report: bool, db_path: &Path) -> Result<()> {
    let store = Store::open(db_path)?;
    let candidates = consolidator::find_candidates(&store, threshold)?;

    if candidates.is_empty() {
        println!("No duplicate pattern candidates found at threshold {:.0}%.", threshold * 100.0);
        return Ok(());
    }

    println!(
        "{} candidate pair(s) above {:.0}% similarity:\n",
        candidates.len(),
        threshold * 100.0
    );
    for (keep_id, discard_id, score, keep_name, discard_name) in &candidates {
        println!(
            "  [{keep_id}] {keep_name}  <=>  [{discard_id}] {discard_name}  ({:.1}%)",
            score * 100.0
        );
    }

    if !report {
        println!("\nMerging: keeping higher-use pattern in each pair...");
        for (keep_id, discard_id, score, keep_name, discard_name) in &candidates {
            consolidator::merge_patterns(&store, *keep_id, *discard_id, *score)?;
            println!("  Merged [{discard_id}] {discard_name} -> [{keep_id}] {keep_name}");
        }
        println!("Done. Run `cortex index` to rebuild FTS from updated patterns.");
    } else {
        println!("\nReport-only mode. No patterns were modified.");
    }
    Ok(())
}

// ── Correction handler ─────────────────────────────────────────────────────────

fn run_correction(
    attempted: &str,
    reason: &str,
    fix: &str,
    tags: &[String],
    db_path: &Path,
) -> Result<()> {
    let store = Store::open(db_path)?;
    let id = store.insert_self_correction(attempted, reason, fix, tags)?;
    println!("Correction recorded (id={id}). Use `cortex anti-pattern add` to promote if this recurs.");
    Ok(())
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
