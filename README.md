# cortex

Persistent semantic memory layer for Copilot. Compresses your codebase into dense
representations, accumulates knowledge across sessions, and serves it as a live MCP
skill — so Copilot spends fewer tokens, asks smarter questions, and remembers what works.

## How it works

```
your source ──► compressor ──► SQLite index
                                    │
         patterns, anti-patterns ───┤
         annotations, call log  ───┤
                                    │
                               MCP server
                                    │
                               Copilot Chat
```

Nothing gets written to memory without your explicit approval.

---

## Setup

```sh
cargo install --path /path/to/cortex

# 1. Index your source (and optionally a quartz-ctx api-graph)
cortex index --source src --api-graph docs/quartz-ctx/api-graph.json --name Quartz

# 2. Start the MCP server (VS Code picks it up from .vscode/mcp.json)
cortex serve --source src --api-graph docs/quartz-ctx/api-graph.json --name Quartz
```

Copy `.vscode/mcp.json` from this repo into your project. VS Code starts cortex
automatically when you open the workspace.

---

## Commands

### Indexing

```sh
cortex index --source src
cortex index --source src --api-graph docs/quartz-ctx/api-graph.json --name Quartz
```

Compresses source files into dense semantic units, stores them in `.cortex/memory.db`.
Re-run after significant API changes.

### Serving (MCP)

```sh
cortex serve --source src --name Quartz
```

Loads the index and serves it as a JSON-RPC MCP server over stdio. Copilot calls
it as a live skill. Copilot tools available:

| Tool | What Copilot can ask |
|------|---------------------|
| `semantic_search` | "Find anything related to collision" |
| `get_item` | "Show me the full details of `Action`" |
| `get_context` | "Give me context for working on src/player.rs" |
| `recall` | "What do we know about gravity?" |
| `list_patterns` | "What patterns are approved?" |
| `get_anti_patterns` | "What should I never do?" |
| `suggest_pattern` | Queue a pattern for your review (never auto-saves) |
| `list_all` | "List all enums in the index" |

### Watching

```sh
cortex watch --source src
```

Observes file changes and queues them as pending observations. **Never auto-approves
anything.** You review and decide what gets remembered.

### Reviewing

```sh
cortex review
```

Lists pending observations from `watch` or Copilot's `suggest_pattern` calls.

### Crystallizing (your decision only)

```sh
# Promote an observation to an approved pattern
cortex crystallize 3 --name "Grounded sound" \
  --intent "Play a sound when an entity lands" \
  --uses "Action::PlaySound,Condition::Grounded" \
  --tags "audio,physics"

# Discard an observation
cortex dismiss 3
```

### Patterns

```sh
cortex pattern list
cortex pattern add --name "..." --intent "..." --body "..."
cortex pattern remove 2
```

### Anti-patterns

```sh
cortex anti-pattern list
cortex anti-pattern add \
  --description "Don't hardcode asset paths" \
  --wrong 'Action::PlaySound { path: "sounds/jump.ogg", volume: 1.0 }' \
  --correct "Use a named constant or asset key from the asset index"
cortex anti-pattern remove 1
```

### Annotations

Free-form notes Copilot will see when the topic is relevant:

```sh
cortex annotate list
cortex annotate add \
  --topic "SetGravity" \
  --body "Gravity is in pixels/sec². Default is 980.0. Values above 2000 cause tunneling." \
  --tags "physics,gotcha"
cortex annotate remove 1
```

### Context packet

Pre-compile context for a task without running the MCP server:

```sh
cortex context "working on player jump mechanics" --token-budget 1500
```

### Status

```sh
cortex status
```

Shows unit count, pattern count, pending observations, and most-called MCP tools.

---

## quartz-ctx integration

cortex reads `docs/quartz-ctx/api-graph.json` directly — no subprocess, no coupling.
Run `quartz-ctx generate` first, then `cortex index --api-graph docs/quartz-ctx/api-graph.json`.
The api-graph items take precedence over raw source units when both exist for the same type
(api-graph has richer doc comments and pre-extracted variant shapes).

---

## copilot-instructions.md snippet

Add this to your existing `.github/copilot-instructions.md`:

```markdown
## Cortex (Semantic Memory)

Before writing any Quartz code:
1. Call `get_anti_patterns` — never violate these.
2. Call `semantic_search` with your intent to find relevant API items.
3. Call `recall` on any type you're about to use.
4. Call `get_context` with the current file paths if starting a new task.

If you generate a useful pattern, call `suggest_pattern` to queue it for review.
Do not assume any pattern is approved until you've seen it in `list_patterns`.
```

---

## Token efficiency

cortex compresses a 400-line Rust struct to ~8 lines of dense semantic signal.
The `get_context` tool pre-selects only what's relevant to the current task,
capping at your token budget. Over time, the call log reveals what Copilot
reaches for most — which informs what to pre-inject and what to annotate.
