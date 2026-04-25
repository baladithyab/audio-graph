# AudioGraph — Documentation Index

Entry point for all AudioGraph documentation. See the main [`README`](../README.md) for setup and quick start.

## Architecture and design

- [`ARCHITECTURE.md`](ARCHITECTURE.md) — 4-thread pipeline model, provider abstraction, event flow.
- [`designs/provider-architecture.md`](designs/provider-architecture.md) — target provider-abstraction design across pipeline stages.
- [`designs/provider-refactor.md`](designs/provider-refactor.md) — refactor plan toward that target.
- [`designs/session-management.md`](designs/session-management.md) — session lifecycle, persistence, recovery.
- [`MODEL_MANAGEMENT_DESIGN.md`](MODEL_MANAGEMENT_DESIGN.md) — model download, caching, and in-app management.
- [`SETTINGS_DESIGN.md`](SETTINGS_DESIGN.md) — Settings page architecture and credential storage.
- [`SYSTEM_TRAY_WIDGET_PROPOSAL.md`](SYSTEM_TRAY_WIDGET_PROPOSAL.md) — proposed system tray widget.
- [`GEMINI_LANGUAGES.md`](GEMINI_LANGUAGES.md) — Gemini Live language coverage.

## Operations

- [`ops/gemini-reconnect-runbook.md`](ops/gemini-reconnect-runbook.md) — Gemini Live reconnect / recovery runbook.

## Reviews and retrospectives

- [`reviews/`](reviews/) — loop-by-loop code review notes
  (`audio-graph-review-loop10.md` through `audio-graph-review-loop23.md`)
  plus the wave-based follow-ups (`audio-graph-wave-a-review.md`,
  `audio-graph-wave-b-review.md`).
- [`reviews/gap-analysis.md`](reviews/gap-analysis.md) — outstanding gaps across the product.
- [`reviews/ux-first-run-review.md`](reviews/ux-first-run-review.md) — first-run UX audit.

## Documentation audits

- [`audit/docs-queue.md`](audit/docs-queue.md) — recursive doc-gap queue
  driving the latest sweep. Records what was queued, in progress, and
  done.

## Release and contributing

- [`RELEASE.md`](RELEASE.md) — release and versioning process.
- [`CONTRIBUTING.md`](CONTRIBUTING.md) — branch workflow, commit conventions, pre-submit checklist.
