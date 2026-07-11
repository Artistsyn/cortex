# Phase 5: Meta-Learning Loop — Finalized Plan

**Date:** 2026-07-11  
**Status:** Proposal for review  
**Target:** `cortex` crate, `synful` branch

---

## 0. Executive Summary

Add a **meta-learning stage** to the consolidation pipeline that analyzes the pipeline's own output and proposes adjustments to thresholds, preferences, and instruction files. Every meta-proposal follows the same review flow as existing proposals (staged, pending, approved/rejected) with a **higher evidence bar** — no auto-tuning, no source code changes.

---

## 1. Audit Findings: Current Limitations

### 1.1 Critical: `recurrent_think` confidence scoring is broken

The `critique_hypothesis()` function checks only **anti-pattern keyword matches** + **graph conflict string containment**. A hypothesis that contains no anti-pattern keywords gets **100% confidence** and halts immediately. This means the recurrent loop cannot meaningfully critique conceptual proposals — it's only useful for flagging concrete code pattern violations.

**Fix (pre-requisite to meta-loop):** Add a real evaluation layer that scores conceptual quality, not just anti-pattern avoidance.

### 1.2 No feedback on proposal effectiveness

Proposals have `status` (pending/approved/rejected) but there is **zero analysis** of:
- What proposal types get approved vs rejected most often
- Which gates fire most often (rejection log is written but never read back)
- Whether approved proposals actually improve outcomes
- How pipeline thresholds should be adjusted based on observed results

### 1.3 Static thresholds

Every configurable value in `prefs.toml` is set once and never adapted:
- `skill_candidate_min_occurrences = 3` — but is 3 the right number?
- `staleness_hours = 8` — should this vary by session frequency?
- Credibility gate minimum of 0.2 — is this too low/high?

### 1.4 No cross-pipeline aggregation

Each pipeline run is independent. There's no query that says "in the last 30 days, 80% of gap proposals were rejected by the credibility gate — suggest raising the threshold."

### 1.5 Session store integration is fragile

`extract_session_markers()` in `closeout.rs` fails silently when the VS Code internal session store is unavailable. This means knowledge markers from conversation text are only captured if the session store happens to be accessible.

### 1.6 Fidelity scoring is simplistic

`score_process_fidelity` in `verify.rs` checks a fixed set of 6 steps with equal weight (0.166 each). Missing steps are equally penalized regardless of importance. No context-aware adjustment.

---

## 2. External Patterns Researched

| System | Pattern | Applicability |
|---|---|---|
| **SkillOpt (Microsoft)** | Trajectory mining → validation-gated edits → deployable skills | Already implemented in Phase 1. Meta-loop extends validation to propose config changes. |
| **AutoML / HPO** | Bayesian optimization over hyperparameters with reward signal | **Partial.** We lack a reward signal (no "did this proposal help?" metric). Design around this by using approval rates as proxy. |
| **MLOps Data Drift** | Monitor input distribution changes, alert when drift exceeds threshold | Already partially implemented (Phase 3 graph drift). Extend to "pattern drift" — are stored patterns still accurate? |
| **Dependabot/Renovate** | Auto-generated PRs with CI verification, never auto-merge | **Directly applicable.** Meta-proposals should create reviewable artifacts (PRs, proposal files) that require human approval. |
| **Kubernetes Operator** | Desired state reconciliation loop | `consolidate-if-stale` is a primitive operator. Meta-loop adds a reconciliation sub-loop. |
| **RLHF** | Human feedback as reward signal | Proposal `status` field (approved/rejected) is exactly this. Use it to train threshold adaptation. |
| **Chaos Engineering** | Intentionally inject failures to test resilience | Could inject a borderline proposal to verify gates catch it — low priority. |

---

## 3. Proposed Architecture

### 3.1 New Module: `src/meta.rs`

A compact module (~300 lines) with four analyzers and one pipeline stage:

```
meta.rs
├── structs: MetaSignal, MetaProposal, MetaReport
├── analyze_rejection_rates()   — reads rejected-proposals.jsonl + proposals table
├── analyze_fidelity_trends()   — reads session_snapshots fidelity scores
├── analyze_gap_evolution()     — reads query_gap_log for hot/unresolved gaps
├── analyze_threshold_impact()  — correlates threshold values with pipeline outcomes
└── run_meta_stage()            — Stage 8 entry point, calls analyzers, stages proposals
```

### 3.2 Data Sources

| Source | Table/File | Analyzer |
|---|---|---|
| Rejection log | `.cortex/rejected-proposals.jsonl` | `analyze_rejection_rates` |
| Proposal outcomes | `proposals` table (status) | `analyze_rejection_rates` |
| Fidelity scores | `session_snapshots` (marker_counts.fidelity_score) | `analyze_fidelity_trends` |
| Query gaps | `query_gap_log` | `analyze_gap_evolution` |
| Pipeline timing | `annotations` (consolidation-last-run) | `analyze_threshold_impact` |

### 3.3 Meta-Proposal Types

| Type | Target | Trigger |
|---|---|---|
| `meta_threshold` | `.cortex/prefs.toml` | Gate rejection rate > 60% for that proposal type |
| `meta_instruction` | `.github/copilot-instructions.md` | Fidelity step consistently missing across 5+ sessions |
| `meta_gap_priority` | `.cortex/prefs.toml` | Same gap query unresolved for 30+ days |
| `meta_pipeline` | internal | Pipeline consistently runs stale or generates no proposals |

### 3.4 Higher Evidence Bar

Meta-proposals require **3× the evidence** of regular proposals:

| Check | Regular | Meta |
|---|---|---|
| Minimum occurrences | 3 sessions | 5 sessions across ≥2 distinct weeks |
| Credibility floor | 0.2 | 0.4 |
| Trial period | 7 days (gap) | 14 days (all types) |
| Approval required | 1 human | 1 human + no rejection in 30 days |

### 3.5 Safety Boundaries

**Can propose changes to:**
- `.cortex/prefs.toml` — threshold values, mode flags
- `.github/copilot-instructions.md` — protocol steps, usage notes
- `agent_customization/skills/` — skill file revisions

**Cannot propose changes to:**
- Any `cortex/src/*.rs` source file — no self-modifying code
- Any `.vscode/` config — too risky
- Any git configuration

---

## 4. Pre-Requisite Fix: Recurrent Confidence Scoring

Before the meta-loop is useful, `recurrent_think` needs a real critique engine:

### 4.1 Current State (Broken)

```rust
// Current: only checks anti-pattern keyword matches
fn critique_hypothesis(hypothesis: &str, conn: &Connection) {
    for (wrong, right) in anti_patterns {
        if hypothesis.contains(&wrong) { /* flag */ }
    }
    // confidence = 1.0 - (critiques / total_checks)
    // No anti-patterns matched → 100% confidence → halt immediately
}
```

### 4.2 Proposed Fix

Add three evaluation dimensions:

| Dimension | Method | Weight |
|---|---|---|
| **Anti-pattern compliance** | Current keyword check (Phase 4 negation-aware) | 30% |
| **Internal consistency** | Check for contradictory statements in hypothesis | 25% |
| **Completeness** | Check for required sections (e.g., "Context:", "Decision:", "Trade-offs:") | 25% |
| **Grounding** | Check for evidence references (e.g., file paths, tool names) | 20% |

This produces a **weighted score** where a hypothesis can be confident in some dimensions and not others. Halting requires ≥0.85 **average across all dimensions**, not just anti-pattern compliance.

---

## 5. Implementation Plan

### Phase 5a: Fix recurrent confidence (pre-requisite)
- Add `evaluate_dimension_*()` functions to `reasoner/recurrent.rs`
- Add `weighted_confidence()` that combines all four dimensions
- Add 4+ new tests
- **Estimate: ~120 lines, 1 session**

### Phase 5b: Meta module foundation
- Create `src/meta.rs` with core structs
- Implement `analyze_rejection_rates()` — queries proposals + rejected-proposals.jsonl
- Implement `run_meta_stage()` — Stage 8 entry point
- Wire into `consolidator2.rs` as Stage 8
- Add `Meta` CLI subcommand group (`cortex meta status`, `cortex meta report`)
- **Estimate: ~300 lines, 1-2 sessions**

### Phase 5c: Advanced analyzers
- Implement `analyze_fidelity_trends()`
- Implement `analyze_gap_evolution()`
- Implement `analyze_threshold_impact()`
- Add drift + fidelity cross-analysis (e.g., "community with high drift also has low fidelity")
- **Estimate: ~200 lines, 1 session**

### Phase 5d: Meta feedback loop to prefs
- `cortex meta apply <id>` — applies an approved meta-proposal to prefs.toml
- `cortex meta dry-run <id>` — show diff without applying
- Integration with `cortex.ps1` launcher
- **Estimate: ~100 lines, 1 session**

### Total: ~720 lines, 4-5 sessions

---

## 6. Risks and Mitigations

| Risk | Likelihood | Mitigation |
|---|---|---|
| Meta-proposals suggest bad threshold changes | Medium | 3× evidence bar + 30-day no-rejection window |
| Feedback loop: meta-proposal changes threshold, causing new meta-proposals | Low | Meta-proposals don't affect meta-analysis itself (no meta-meta) |
| Analysis noise from sparse data | Medium | Minimum 5 sessions across 2 weeks before any meta-proposal |
| User confusion: "why is cortex suggesting changes to itself?" | Medium | All meta-proposals clearly labeled as `meta_*` type with description field explaining evidence |
| Threshold change breaks existing behavior | Low | Changes go through same review flow — not auto-applied |

---

## 7. Discussion Questions

1. **Reward signal:** Should we add a "helpful" field to proposals that the user sets after experiencing the result? This would give us a true RLHF signal.
2. **PR generation:** Should approved meta-proposals auto-create a GitHub PR (like Dependabot) or just stage a proposal file?
3. **Meta urgency:** Should high-drift communities trigger expedited meta-review, or is standard pipeline cadence sufficient?
4. **Retroactive analysis:** Should the first `cortex meta report` include analysis of *all* historical proposals, or only new ones going forward?
5. **Plugin architecture:** Eventually, should meta-analyzers be pluggable (via `Action::PluginCall` style dispatch) so third parties can add custom meta signals?
