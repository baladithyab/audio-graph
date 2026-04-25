# AudioGraph documentation audit queue

This queue tracks the recursive doc-gap audit of the audio-graph submodule.
New gaps discovered while working an item get appended to **TODO**.

## Scope

`/apps/audio-graph/` — Tauri v2 + React app. Rust backend under `src-tauri/`,
React frontend under `src/`, docs under `docs/`.

Stop criteria: TODO empty AND final re-survey finds no new gaps.

## Legend

- **TODO**: queued, not yet started
- **DOING**: in progress
- **DONE**: finished

---

## TODO

_(empty — final re-survey complete, no new gaps)_

## DOING

_(none)_

## DONE

### Wave 1 — Survey

- Full survey of `src-tauri/src/`, `src/`, `docs/`, and review docs.
  Result: every Rust module already had a `//!` header (error, events,
  state, commands, asr, audio, aws_util, crash_handler, credentials,
  diarization, fs_util, gemini, graph, llm, logging, models, persistence,
  sessions, settings, speech) and every custom hook already had full
  JSDoc. Real gaps: React components lacked top-of-file JSDoc blocks
  explaining purpose / props contract / parent-child relationships; a
  few cross-cutting TS files (store, types, useTauriEvents) lacked
  file-scope docs; and `src-tauri/src/asr/cloud.rs` was the one Rust
  file missing a `//!` header.

### Wave 2 — React component JSDoc

Added top-of-file JSDoc blocks covering purpose, composition, store
bindings, parent relationship, and props contract to:

- `src/App.tsx`
- `src/components/ControlBar.tsx`
- `src/components/AudioSourceSelector.tsx`
- `src/components/ChatSidebar.tsx`
- `src/components/KnowledgeGraphViewer.tsx`
- `src/components/LiveTranscript.tsx`
- `src/components/SpeakerPanel.tsx`
- `src/components/PipelineStatusBar.tsx`
- `src/components/SettingsPage.tsx`
- `src/components/SessionsBrowser.tsx`
- `src/components/TokenUsagePanel.tsx`
- `src/components/StorageBanner.tsx`
- `src/components/Toast.tsx`
- `src/components/ExpressSetup.tsx`
- `src/components/ShortcutsHelpModal.tsx`
- `src/components/CredentialsManager.tsx`
- `src/components/AsrProviderSettings.tsx`
- `src/components/LlmProviderSettings.tsx`
- `src/components/GeminiSettings.tsx`
- `src/components/settingsTypes.ts`

### Wave 3 — Frontend cross-cutting files

- `src/store/index.ts` — module header covering the Zustand slice layout
  and invoke-bridge contract.
- `src/types/index.ts` — module header describing the IPC contract
  boundary and the `ALLOWED_CREDENTIAL_KEYS` sync requirement.
- `src/hooks/useTauriEvents.ts` — file-level header listing every
  subscribed event and its store/publisher routing.

### Wave 4 — Backend gap

- `src-tauri/src/asr/cloud.rs` — added `//!` header describing the
  generic cloud-ASR worker contract and its relationship to the
  WebSocket-based provider backends (Deepgram, AssemblyAI, AWS
  Transcribe).

### Wave 5 — Rustdoc warnings sweep

`cargo doc --no-deps` reported 13 warnings at the start of the audit
(pre-existing, not introduced here, but in scope for "deeply document"
since they meant broken intra-doc links). All fixed:

- `credentials/mod.rs` — `[` `]` around `set_field` / `is_allowed_key`
  pointed at private items; switched to plain backticks where the
  target is private, kept `[...]` only for public items.
- `events.rs::AwsErrorPayload` — `[`UiAwsError`]` → fully qualified
  `[`crate::aws_util::UiAwsError`]`.
- `gemini/mod.rs` — two broken links: `open_ws` (private fn) and
  `session_task` (private fn) → plain backticks. `[`Auth`]` referred
  to a sibling enum variant in the same enum → `[`Self::Auth`]`.
- `graph/temporal.rs::process_extraction` — `MAX_NODES` / `MAX_EDGES`
  are private consts in the same module → plain backticks.
- `llm/engine.rs` — `[`LlamaContext`]` is a re-export from
  `llama_cpp_2` not visible to rustdoc → plain backticks with crate
  attribution inline.
- `persistence/io.rs` — two `CAPTURE_STORAGE_FULL` links re-qualified
  via the `[`NAME`](crate::events::NAME)` form so they resolve.
- `persistence/mod.rs::save_json` — `[`io::handle_write_error`]` was
  private → plain backticks.
- `graph/extraction.rs` — `<Place>` inside a doc comment was parsed
  as an unclosed HTML tag; wrapped the examples in inline code so
  rustdoc treats them as literals.

After the sweep: `cargo doc --no-deps --manifest-path
.../src-tauri/Cargo.toml` produces zero warnings (the only stderr
line is rsac's `Building for macOS 26.4.1` build-script info, which
is not a rustdoc warning).

### Wave 6 — Cross-reference cleanup

- `docs/README.md` — updated the "Reviews and retrospectives" section
  to reflect the current loop range (10–23) plus the wave-a and
  wave-b reviews; added a pointer to the new `audit/docs-queue.md`.

### Wave 7 — Final re-survey

- Re-checked the Rust module tree (all `//!` headers intact; no new
  modules added during the audit). No further gaps.
- Re-checked the React component tree — every `.tsx` now has a
  top-of-file JSDoc. No further gaps.
- Cross-referenced docs against the wave-a and wave-b review docs;
  no contradictions found with the ratified design decisions
  (provider-architecture path, persistence session-rotation,
  Gemini reconnect semantics, AWS refreshing-credentials provider).
- `cargo fmt --check`, `cargo check`, `cargo clippy -D warnings`,
  `cargo doc --no-deps` all pass with zero warnings.
</content>
