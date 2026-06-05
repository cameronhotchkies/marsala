# Marsala Planning Artifacts

This directory captures the planning pass for `marsala`, a local-first Rust LLM proxy.

Files:

- `01_pm_plan.md`: initial project-manager plan.
- `02_devils_advocate_critique.md`: engineering critique of the initial direction.
- `03_reconciled_plan.md`: final plan after the PM and critic reconciled.
- `04_elevator_pitch_hit_list.md`: short positioning and pitch bullets.

The reconciled plan is the source of truth for implementation sequencing.

Current planning baseline:

- The shipped non-streaming `/v1/chat/completions` proxy is retained as a compatibility scaffold.
- The next product-defining milestone is Codex traffic acquisition/interception viability, including routing, auth, streaming shape, and safe logging defaults.
- Proxy and CLI interception validation now come before streaming mutation, rewrite semantics, and tool-call capture work.
- The planning set does not claim Marsala intercepts Codex today.
