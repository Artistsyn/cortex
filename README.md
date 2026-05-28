# cortex

Persistent semantic memory layer for Copilot. Compresses your codebase into dense
representations, accumulates knowledge across sessions, and serves it as a live MCP
skill - so Copilot spends fewer tokens, asks smarter questions, and remembers what works.

## How it works

```
your source --> compressor --> SQLite index
                                    |
         patterns, anti-patterns ---|
         annotations, call log  ---|
                                    |
                               MCP server
                                    |
                               Copilot Chat
```

Nothing gets written to memory without your explicit approval.

---

## Setup

```sh
cargo install --path /path/to/cortex

# 0. First-time workspace bootstrap (creates .cortex/cortex.ps1, .cortex/index-sources.json, .vscode/mcp.json)
cortex bootstrap --repo . --source src --name MyProject

# 1. Index your source (and optionally a quartz-ctx api-graph)
cortex index --source src --api-graph docs/quartz-ctx/api-graph.json --name Quartz

# 2. Start the MCP server (VS Code picks it up from .vscode/mcp.json)
cortex serve --source src --api-graph docs/quartz-ctx/api-graph.json --name Quartz
```

The bootstrap command writes a valid direct-binary Cortex MCP entry. It avoids
mixed command/argument family bugs (for example, `cortex.exe` command with
PowerShell `-File` arguments).

### Copilot Chat MCP readiness (required)

Before relying on Cortex in chat, verify the required MCP baseline is callable:
- `get_delta`
- `get_preferences`
- `get_anti_patterns`
- `list_patterns`
- `get_context`

If any required tool is missing/failing, remediate before coding:

```powershell
.\.cortex\cortex.ps1 doctor --format json
.\.cortex\cortex.ps1 -- status --format json --full
```

Then verify `.vscode/mcp.json` has a Cortex server entry, restart the server path,
and reload VS Code window if needed:

```powershell
.\.cortex\cortex.ps1 serve
```

Do not proceed with non-trivial tasks until the MCP baseline passes (unless user
explicitly approves degraded mode after a blocker report).

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
| `get_item` | "Show the full details of `Action`" |
| `get_context` | "Give me context for working on src/player.rs" |
| `get_delta` | "Show changes since last checkpoint, excluding build artifacts" |
| `recall` | "What do we know about gravity?" |
| `list_patterns` | "What patterns are approved?" |
| `get_anti_patterns` | "What should I never do?" |
| `suggest_pattern` | Queue a pattern for your review (never auto-saves) |
| `list_all` | "List all enums in the index" |

Phase 4.2 delta controls:

- `get_delta`: `include`, `exclude`, `max_files`, `max_patch_lines`
- `get_context`: `delta_include`, `delta_exclude`, `delta_max_files`, `delta_max_patch_lines`

### Watching

```sh
cortex watch --source src
```

Observes file changes and queues them as pending observations. Never auto-approves
anything. You review and decide what gets remembered.

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

# Script-safe mode
cortex --format json pattern list
cortex --format json pattern health
```

### Anti-patterns

```sh
cortex anti-pattern list
cortex anti-pattern add \
  --description "Don't hardcode asset paths" \
  --wrong 'Action::PlaySound { path: "sounds/jump.ogg", volume: 1.0 }' \
  --correct "Use a named constant or asset key from the asset index"
cortex anti-pattern remove 1

# Script-safe mode
cortex --format json anti-pattern list
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

# Script-safe mode
cortex --format json annotate list
```

### Context packet

Pre-compile context for a task without running the MCP server:

```sh
cortex context "working on player jump mechanics" --token-budget 1500
cortex context "working on player jump mechanics" --delta-exclude flowmango-demo --delta-max-files 8
```

### Status

```sh
cortex status
cortex --format json status --full
```

Shows unit count, pattern count, pending observations, and most-called MCP tools.

### Workflow Doctor (Phase 4.2)

Production-style smoke validation for automation pipelines:

```sh
# Non-mutating workflow checks (safe default)
cortex doctor workflow --repo . --source src --name Quartz

# JSON output for scripts/CI
cortex --format json doctor workflow --repo . --source src --name Quartz

# Optional mutation roundtrip (adds/reverts/removes a sentinel pattern)
cortex doctor workflow --repo . --source src --mutate-pattern
```

Doctor checks include index presence, delta query health, context packet generation,
and status rendering. It exits non-zero if any check fails.

---

## quartz-ctx integration

cortex reads `docs/quartz-ctx/api-graph.json` directly - no subprocess, no coupling.
Run `quartz-ctx generate` first, then `cortex index --api-graph docs/quartz-ctx/api-graph.json`.
The api-graph items take precedence over raw source units when both exist for the same type
(api-graph has richer doc comments and pre-extracted variant shapes).

---

## copilot-instructions.md snippet

Add this block to your `.github/copilot-instructions.md`. It teaches the assistant
when and how to use cortex throughout a session — not just at boot.

```markdown
## Cortex (Semantic Memory Layer)

cortex holds project-specific knowledge that is NOT in training data:
bug traps, approved patterns, API facts, architecture decisions, and corrections.
Always consult it before writing code and when blocked during a task.

### PROTOCOL - CORTEX Trigger

If user message contains PROTOCOL - CORTEX -:
- Run MCP readiness gate first: required tools are get_delta, get_preferences,
  get_anti_patterns, list_patterns, get_context
- If any required tool fails, run remediation loop before coding:
  1) `.\.cortex\cortex.ps1 doctor --format json`
  2) `.\.cortex\cortex.ps1 -- status --format json --full`
  3) verify `.vscode/mcp.json` cortex server entry
  4) restart MCP server path (`.\.cortex\cortex.ps1 serve`) and re-probe tools
  5) reload VS Code window and re-probe
- Run baseline retrieval: get_delta → get_preferences → get_anti_patterns → get_context
- Use JSON mode for automation-critical commands: cortex --format json status --full
- Hard rule: do not silently bypass missing required MCP tools for non-trivial tasks.
  Stop and report blocker unless user explicitly approves degraded mode.

### Mandatory Pre-Code Check (no trigger required)

Before writing any factory, tick/update, spawn, pool, or physics-integration function:
1. `get_anti_patterns` — check all known traps for this project
2. `get_preferences` — load current style rules and API notes
3. `list_patterns` — find approved patterns for the task category

Skip only for trivial changes: renaming a constant, fixing a typo, adding a comment.

### Mid-Task Cortex Checkpoints

Cortex is a co-author, not a boot-time shelf. Consult it at every "I'm not sure" moment:

| Situation | Tool to call |
|---|---|
| First approach failed | `recall <error_keyword>` before trying a second approach |
| Unfamiliar compiler error | `semantic_search <error description>` before reading source |
| A type/module behaves unexpectedly | `get_item <typename>` before reading docs |
| About to add a new integration point | `simulate_change <unit>` to preview impact first |
| Code compiles but behavior is wrong | `recall <behavior_keyword>` — may be a known runtime trap |
| Choosing between two approaches | `list_patterns` + `get_anti_patterns` to see if one is vetted |

**Blocked rule:** After two failed attempts at the same problem, STOP and run
`recall <topic>` before a third. If cortex has nothing, note the gap for crystallization.

### Tagging Quality (for semantic findability)

New cortex entries must be findable by concept, not just exact API name:
- Tags: API name + behavior + domain + colloquial term
  e.g., GrappleConstraint → tags: grapple,hook,rope,constraint,swing,GrappleConstraint
- Include error code if applicable: E0583,file-not-found (not just module-resolution)
- First sentence of description = what goes wrong, not what the feature is
- Body text: include both the official name AND plain-words description
- Use `semantic_search` to look up entries — it uses embedding similarity,
  so conceptual descriptions find relevant entries even with wrong API names

### Session-End (Mandatory)

After every session where code was written or a bug was fixed:
1. Run post-session: `.\.cortex\cortex.ps1 post-session` (or your launcher equivalent)
2. Add new bugs as anti-patterns; working implementations as patterns
3. Update prefs notes if a new API fact was discovered
```

### Recommended initial prefs.toml

Create `.cortex/prefs.toml` in your project root (or run `cortex init` via the launcher):

```toml
[style]
line_length = 100
indent = "4 spaces"
naming = "snake_case functions and variables, PascalCase types and enums"

[project]
name = "YourProject"
language = "Rust"
notes = [
    "MANDATORY PRE-CODE CHECK (no PROTOCOL required): before writing any factory/tick/spawn/physics function call get_anti_patterns + get_preferences + list_patterns",
    "MANDATORY MID-TASK CORTEX USAGE: after first approach fails call recall <error_keyword> before retrying. After two failed attempts STOP and call recall or semantic_search before a third.",
    "session-end mandatory: after any coding session run post-session then annotate new bugs as anti-patterns and working implementations as patterns",
]
```

### Windows / PowerShell CLI notes

When passing strings to `cortex.exe` from PowerShell:
- All `--description`, `--body`, `--reason`, `--wrong`, `--correct` values must be **single-line**
  — multiline string variables pass each newline as a separate argument to the exe
- Use `;` not `&&` for command chaining (PowerShell 5.1 does not support `&&`)
- Use ASCII hyphen `-` not em-dash `—` in argument values
- Use single-quoted `'strings'` for static values; double-quoted strings expand `$vars`
- After any cortex command, check `$LASTEXITCODE` — silent failure is possible

---

## Token efficiency

cortex compresses a 400-line Rust struct to ~8 lines of dense semantic signal.
The `get_context` tool pre-selects only what's relevant to the current task,
capping at your token budget. Over time, the call log reveals what Copilot
reaches for most - which informs what to pre-inject and what to annotate.
