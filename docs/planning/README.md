# Marsala Planning Artifacts

This directory captures the planning pass for `marsala`, a local-first Rust LLM proxy.

Files:

- `01_pm_plan.md`: initial project-manager plan.
- `02_devils_advocate_critique.md`: engineering critique of the initial direction.
- `03_reconciled_plan.md`: current product and implementation sequencing plan.
- `04_elevator_pitch_hit_list.md`: short positioning and pitch bullets.
- `05_mitm_foundation.md`: current MITM mainline architecture and implementation plan.

The reconciled plan and MITM foundation plan are the source of truth for implementation sequencing.

Current planning baseline:

- The explicit OpenAI-shaped gateway is retained as a compatibility scaffold and fixture source.
- The mainline milestone is allowlisted MITM for normal ChatGPT-backed Codex traffic.
- Normal Codex has been observed through Marsala's proxy as tunneled `CONNECT` traffic to `chatgpt.com` and `ab.chatgpt.com`.
- The custom-provider inbound-auth shortcut to `api.openai.com/v1/responses` was rejected upstream with `401 Unauthorized`.
- Marsala does not claim decrypted Codex interception until TLS termination and sanitized `mitm_request` metadata are proven.
