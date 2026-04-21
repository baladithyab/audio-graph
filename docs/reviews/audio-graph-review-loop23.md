# audio-graph Loop 23 Review

**Date:** 2026-04-17  
**Reviewer:** B2  
**Scope:** READ-ONLY code review of apps/audio-graph  
**Focus Areas:** A1 (SettingsPage refactor), A2 (hook tests), tech-debt flagging

---

## Executive Summary

**Status:** In-flight work (A1, A2) NOT YET LANDED on master.

**Current state:**
- SettingsPage.tsx is a monolithic 1910-line component combining all provider UI + state management
- useTauriEvents, useKeyboardShortcuts, useFocusTrap hooks exist and are used correctly in App.tsx
- A2 hook tests are **NOT IN THE CODEBASE** (tasks #1 is pending)
- No A1 sub-component split has been applied yet (task #2 is pending)

**Key findings:**

1. **A1 Refactor Opportunity Confirmed:** SettingsPage has clear separation boundaries for 5 sub-components (AudioSettings, AsrProviderSettings, LlmProviderSettings, GeminiSettings, CredentialsManager). This is sound architecture.

2. **A2 Test Gap:** The three hooks (useTauriEvents, useKeyboardShortcuts, useFocusTrap) have **zero test coverage**. Given their critical role in app initialization and event handling, this is a priority gap.

3. **Event Handler Risks in SettingsPage:** The component correctly uses i18n and dispatch patterns, but state hydration (lines 491–622) is complex; refactoring must preserve all 8 async operations for credential loading.

4. **No Regressions Observed:** SettingsPage properly mirrors AWS credentials between ASR/Bedrock forms (lines 209–226), handles test result lifecycle correctly (debounce, timeout), and clears credentials atomically.

---

## Detailed Analysis

### Focus Area 1: Did A1's Refactor Preserve Behavior? (PREFLIGHT CHECK)

**Finding:** A1 refactor has NOT been applied. SettingsPage remains monolithic. Assuming extraction, here are the critical invariants to preserve:

#### 1.1 Event Handlers & Reducer Actions (Preserved?)

| Handler | Used By | Risk | Mitigation |
|---------|---------|------|-----------|
| `handleSave` | Footer "Save" button (line 1900) | Must marshal ASR+LLM+Gemini+audio state into Tauri invoke | Extract handler logic carefully; keep builder pattern |
| `handleTestAsrApi`, `handleTestDeepgram`, etc. (6 total) | Test buttons in provider sections | Closure over dispatch + t (i18n); debounce on testingKey | Ensure all 6 test handlers moved to parent or wrapper |
| `handleClearCredential` | AWS access_keys "Clear" buttons (2 locations) | Compound invocation: `delete_credential_cmd` → confirmation → dispatch → fallback invoke for session token | Keep as shared utility in parent or context |
| `handleLogLevelChange` | Log level select (line 1873) | Side effect: invoke `set_log_level` immediately + dispatch; **NOT** deferred to Save | Must run eagerly in refactored form |
| `handleDeleteClick` | Model delete button (line 834) | Toggles `confirmDelete` state twice | Preserve two-click confirm UX |

**A1 Requirement:** If SettingsPage splits into sub-components (e.g., AsrProviderSettings, LlmProviderSettings), all these handlers must remain at parent level with proper callback props. **Risk:** If handlers are duplicated in sub-components, AWS credential mirroring (lines 209–226) will break — both ASR and Bedrock forms share awsAsrSecretKey/awsBedrockSecretKey storage.

#### 1.2 i18n Calls (All Reachable?)

SettingsPage has 57+ translation keys across 8 sections (Audio, Models, ASR, LLM, Gemini, Diagnostics, Error messages). A few critical ones:
- `settings.credentialConfirm.clearPrompt` (line 376) — used in `handleClearCredential`
- `settings.errors.testTimeout` (line 349) — in runTest timeout handler
- `settings.errors.failedToClear` (line 384) — credential deletion error

**A1 Requirement:** Sub-components must receive `t` from parent via prop, not re-useTranslation(). This ensures i18n context is shared. **Risk:** Missing or unreachable i18n keys would render [i18n_key] in UI.

#### 1.3 useReducer Dispatch Reachability

The reducer (lines 201–245) handles 8 action types:
- `SET_FIELD` (generic scalar updates) — used 50+ times
- `HYDRATE_FROM_SETTINGS` (batch load from Rust) — line 595
- `SET_AWS_SHARED_SECRET`, `SET_AWS_SHARED_SESSION_TOKEN`, `CLEAR_AWS_SHARED_KEYS` — AWS credential mirroring
- `SET_AWS_PROFILES` — line 325
- `TEST_START`, `TEST_RESULT`, `TEST_FINISH` — test lifecycle
- `SET_CONFIRM_DELETE` — model delete confirmation

**A1 Requirement:** If SettingsPage splits into 5 sub-components, at minimum AsrProviderSettings and LlmProviderSettings must be able to dispatch `SET_AWS_SHARED_SECRET` and the shared AWS credential actions. **Risk:** Sub-components designed in isolation (e.g., each with its own `useReducer`) will fail to sync credentials; the "Save" handler would overwrite one form's values with the other's. Critical invariant: **AWS secret key + session token are shared storage**.

---

### Focus Area 2: Are A2's Hook Tests Comprehensive? (NOT IN CODEBASE)

**Status:** Task #1 (A2: hook tests) is marked `in_progress` but **no test files exist** for the three hooks.

#### 2.1 useTauriEvents (36 lines, 9 event subscriptions)

**Current Implementation (src/hooks/useTauriEvents.ts, lines 45–146):**
- Sets up async subscriptions to 9 Tauri events (TRANSCRIPT_UPDATE, GRAPH_UPDATE, PIPELINE_STATUS, SPEAKER_DETECTED, CAPTURE_ERROR, CAPTURE_BACKPRESSURE, CAPTURE_STORAGE_FULL, GEMINI_TRANSCRIPTION, GEMINI_RESPONSE, GEMINI_STATUS)
- All store actions are extracted once per event type (lines 37–43)
- Cleanup: unlistens are collected and called on unmount (lines 45–146)

**A2 Coverage Gaps (Critical):**

| Test Case | Why It Matters | Current Status |
|-----------|---|---|
| **Cleanup on unmount during subscribe** | If component unmounts while `setup()` is still resolving, unlisten array incomplete → dangling listeners consume memory | MISSING |
| **Multiple event subscriptions** | Does order matter? Do all 9 listeners attach? Check that GEMINI_* payloads match type expectations | MISSING |
| **Store action dispatch correctness** | `addTranscriptSegment`, `setGraphSnapshot`, etc. called with correct payload? Type safety? | MISSING |
| **GEMINI_TRANSCRIPTION ID generation** | Line 99: `id: \`gemini-${Date.now()}-${Math.random().toString(36).slice(2, 8)}\`` — can collide if 2 events within same ms. Acceptable? | NOT TESTED |
| **Error-in-store-action** | If `addTranscriptSegment` throws, does the whole hook break or is it isolated? | MISSING |
| **GEMINI_STATUS state mutation** | Line 127: direct `useAudioGraphStore.setState({ isGeminiActive: false })` — not idempotent if called repeatedly. Expected? | MISSING |

**Recommendation for A2:**
```typescript
describe("useTauriEvents", () => {
  it("sets up 9 listener subscriptions and cleans up on unmount", async () => {
    const unlisten = [];
    const mockListen = vi.fn(async (event, callback) => {
      unlisten.push(vi.fn());
      return unlisten[unlisten.length - 1];
    });
    vi.mocked(listen).mockImplementation(mockListen);
    
    const { unmount } = render(<TestComponent useHook={useTauriEvents} />);
    await waitFor(() => expect(mockListen).toHaveBeenCalledTimes(9));
    
    unmount();
    unlisten.forEach(fn => {
      expect(fn).toHaveBeenCalled();
    });
  });

  it("handles cleanup even if unmount occurs during async setup", async () => {
    // Simulate slow setup: use Promise.delay(1000) in listen
    // unmount immediately → unlistens must still fire when resolve completes
  });

  it("dispatches store actions with correct payloads", async () => {
    const mockAdd = vi.fn();
    vi.spyOn(useAudioGraphStore, 'setState').mockReturnValue({} as any);
    // Fire TRANSCRIPT_UPDATE event → verify addTranscriptSegment called with payload
  });
});
```

#### 2.2 useKeyboardShortcuts (54 lines, 4 shortcuts + 1 modal)

**Current Implementation (src/hooks/useKeyboardShortcuts.ts, lines 19–84):**
- Cmd/Ctrl+R: toggle capture (lines 64–71)
- Cmd/Ctrl+,: open settings (lines 74–77)
- Cmd/Ctrl+Shift+S: open sessions browser (lines 55–58)
- Escape: close any modal (lines 34–45)
- Typing-context guard: skips shortcuts in INPUT, TEXTAREA, contenteditable (lines 23–27)

**A2 Coverage Gaps (Critical):**

| Test Case | Why It Matters | Current Status |
|-----------|---|---|
| **Typing-context skip** | Should NOT trigger shortcuts when in input/textarea. Does `contenteditable` work? | MISSING |
| **Modifier key (mod) guard** | Cmd/Ctrl must be present. Is it checked for all shortcuts? Test metaKey on Mac, ctrlKey elsewhere? | MISSING |
| **Escape is special** | Escape works **even in typing context** (intentional design). Other shortcuts do not. | MISSING |
| **Shift+S vs S distinction** | Cmd+Shift+S opens SessionsBrowser, Cmd+S alone should NOT. Overlapping key logic? | MISSING |
| **Store action dispatch** | `state.stopCapture()`, `state.openSettings()` etc. are async void. Are they awaited? Should they be? | MISSING |
| **Event preventDefault** | All shortcuts call `e.preventDefault()`. Verify this prevents default browser behavior (Cmd+, normally opens Chrome settings on Mac). | MISSING |

**Recommendation for A2:**
```typescript
describe("useKeyboardShortcuts", () => {
  it("skips shortcuts when typing in input", () => {
    const input = document.createElement("input");
    document.body.appendChild(input);
    input.focus();
    
    const fireKeydown = (key: string) => {
      input.dispatchEvent(new KeyboardEvent("keydown", { key, metaKey: true }));
    };
    
    fireKeydown("r");
    // startCapture() should NOT have been called
  });

  it("respects contenteditable guard", () => {
    const div = document.createElement("div");
    div.contentEditable = "true";
    document.body.appendChild(div);
    div.focus();
    
    window.dispatchEvent(new KeyboardEvent("keydown", { key: "r", metaKey: true }));
    // startCapture() should NOT have been called
  });

  it("Escape closes settings even while typing", () => {
    const input = document.createElement("input");
    document.body.appendChild(input);
    input.focus();
    
    // Simulate Escape
    window.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape" }));
    // closeSettings() SHOULD have been called
  });

  it("handles Cmd+Shift+S correctly (sessions), not Cmd+S (settings)", () => {
    // Shift + S → sessions browser
    // Plain S (with Cmd) should not match
  });

  it("calls preventDefault on all shortcuts", () => {
    const e = new KeyboardEvent("keydown", { key: "r", metaKey: true });
    const spy = vi.spyOn(e, "preventDefault");
    
    // Trigger handler
    window.dispatchEvent(e);
    expect(spy).toHaveBeenCalled();
  });
});
```

#### 2.3 useFocusTrap (57 lines, focus management for modals)

**Current Implementation (src/hooks/useFocusTrap.ts, lines 25–54):**
- On mount: saves focused element, moves focus into modal (prefer container with tabIndex, fallback to first focusable child)
- On unmount: restores focus to previously-focused element (if it still exists in DOM)
- Intentionally NOT a full focus trap (no Tab cycling)

**A2 Coverage Gaps (Critical):**

| Test Case | Why It Matters | Current Status |
|-----------|---|---|
| **Focus moves on mount** | Modal container receives focus. If `tabindex={-1}`, focus goes there; otherwise first focusable child (button, input, etc.). | MISSING |
| **Focus restored on unmount** | If modal closes, focus returns to element that opened it. Edge case: opener was unmounted? Should gracefully skip. | MISSING |
| **Backward/Forward cycling** | Hook says "NOT a full focus trap". But does Tab still navigate inside modal? If yes, can user Tab into the background page? | MISSING |
| **Opener unmounted while modal open** | Line 50: guard checks `document.contains(prev)`. Does this actually prevent focus() on missing element? | MISSING |
| **contentEditable child** | If modal contains contenteditable div, should focus go there? Currently querySelector looks for 'button, [href], input, select, textarea, [tabindex]...' | MISSING |

**Recommendation for A2:**
```typescript
describe("useFocusTrap", () => {
  it("moves focus to container on mount if tabIndex present", () => {
    const { getByRole } = render(
      <div ref={ref} tabIndex={-1} role="dialog">
        <button>Test</button>
      </div>
    );
    
    expect(document.activeElement).toBe(getByRole("dialog"));
  });

  it("falls back to first focusable child if container not focusable", () => {
    const { getByRole } = render(
      <div ref={ref} role="dialog">
        <button>Test Button</button>
        <input />
      </div>
    );
    
    expect(document.activeElement).toBe(getByRole("button"));
  });

  it("restores focus to opener on unmount", () => {
    const opener = document.createElement("button");
    document.body.appendChild(opener);
    opener.focus();
    
    const { unmount } = render(<ModalWithFocusTrap />);
    unmount();
    
    expect(document.activeElement).toBe(opener);
  });

  it("skips focus restore if opener unmounted", () => {
    const opener = document.createElement("button");
    document.body.appendChild(opener);
    opener.focus();
    
    const { unmount } = render(<ModalWithFocusTrap />);
    document.body.removeChild(opener);
    
    expect(() => unmount()).not.toThrow();
  });

  it("does NOT prevent Tab navigation inside modal (not full trap)", () => {
    const { getByRole } = render(
      <div ref={ref} role="dialog">
        <button>Button 1</button>
        <button>Button 2</button>
      </div>
    );
    
    const btn1 = getByRole("button", { name: "Button 1" });
    btn1.focus();
    
    fireEvent.keyDown(btn1, { key: "Tab" });
    // Focus should move to Button 2 (or background if it's the last)
    // This is NOT trapped — that's by design.
  });
});
```

---

### Focus Area 3: Tech-Debt Candidates for Post-v0.1.0

#### 3.1 Session ID Rotation Hardening

**Current State:**
- SettingsPage allows user to Save/Close without affecting session ID
- App.tsx has `getSessionId()` (line 80) but it's never called in render path
- StorageBanner monitors storage events but doesn't rotate session on new capture

**Why It Matters:**
- Users can export transcript + graph mid-session
- If capture is restarted without session rotation, new data appends to same session
- No explicit "New Session" button or auto-rotation on capture start

**Recommendation for Post-v0.1.0:**
```
Feature: "New Session" button on ControlBar or Settings
- On click: reset transcriptSegments, graphSnapshot; invoke backend to rotate session_id
- On capture start: auto-rotate if session already has >N events (configurable)
- Test: sessionId changes after rotation, no data loss
```

#### 3.2 Offline / Demo Mode

**Current State:**
- All Tauri invokes are fire-and-forget (no fallback if backend down)
- Settings load is blocking (`if (!settings) return`)
- No demo/mock data path for testing without Rust backend

**Why It Matters:**
- Developing UI without running full Rust backend is cumbersome
- Crash logs show "invoke failed: [backend error]" with no graceful UX
- Offline-first design (local audio capture works, cloud ASR queued) not visible in UI

**Recommendation for Post-v0.1.0:**
```
Feature: Offline/Demo mode toggle in Settings → Diagnostics
- If enabled: mock all Tauri invokes with simulated data
- Mock transcripts: "Hello world, this is a demo transcript."
- Mock graph: synthetic entity + link updates every 100ms
- Test: UI renders identically in offline mode
```

#### 3.3 Session Browser UX Refresh

**Current State:**
- SessionsBrowser component exists but minimal in review scope (read-only)
- No session filtering, sorting, or search
- Session deletion requires two-click confirm (same as model deletion)

**Why It Matters:**
- Users with 100+ sessions have no way to find old ones quickly
- Accidental session delete leaves no audit trail (immediate deletion)

**Recommendation for Post-v0.1.0:**
```
Enhancement: SessionsBrowser v2
- Add search by transcript keywords (real-time client-side grep)
- Sort by date, transcript length, top entities
- Soft-delete: move to trash folder; restore within 7 days; hard-delete with confirmation
- Test: Search finds sessions, sort is stable, undo works
```

---

## Critical Invariants to Preserve in A1/A2

| Invariant | Why | How to Test |
|-----------|-----|-------------|
| AWS credential mirroring (ASR ↔ Bedrock share secret key + token) | Saving with ASR access_keys mode then switching to Bedrock must show same secret | `handleSave` invokes credential store once, both forms read same keys |
| i18n context is shared across sub-components | Translation keys must resolve; missing keys break UI | Sub-components receive `t` prop from parent |
| useReducer state is single source of truth | Multiple dispatch sites must stay in sync | No duplicated state between parent + sub-components |
| Test timeout is enforced (10s, line 331) | Hung network calls don't lock the UI | `runTest` wraps invocation in `Promise.race` with timeout |
| Event cleanup on unmount (useTauriEvents) | No dangling listeners after component unmounts | Cleanup function (line 145) called on effect unmount |
| Typing-context guard works everywhere (useKeyboardShortcuts) | Shortcuts don't fire while editing credentials | Test INPUT, TEXTAREA, contenteditable all skip shortcuts |
| Focus restore after modal close (useFocusTrap) | Keyboard users can navigate back to the button that opened the modal | Previous element focus restored unless it was removed |

---

## Top 3 Recommendations for Loop 24

### 1. **Complete A2: Ship Hook Tests (15–20 tests)**
   - **Scope:** useTauriEvents (5 tests), useKeyboardShortcuts (5 tests), useFocusTrap (5 tests)
   - **Key gaps:** Cleanup under unmount, typing-context guards, edge cases (opener removed, state collision)
   - **Acceptance:** All 15–20 tests passing, coverage >90% for hook logic
   - **Risk:** Without tests, regressions in event subscription or focus management won't surface until production

### 2. **Complete A1: Refactor SettingsPage into 5 Sub-Components**
   - **Scope:** AudioSettings, AsrProviderSettings, LlmProviderSettings, GeminiSettings, CredentialsManager
   - **Critical:** Preserve reducer dispatch patterns, AWS credential mirroring, i18n context
   - **Acceptance:** Behavior identical to current monolithic form, file size reduced, tests pass
   - **Risk:** If AWS credential logic is duplicated, Save will break; if i18n not passed as prop, translations vanish

### 3. **Review Post-v0.1.0 Tech-Debt Issues**
   - **Open issues for Loop 25+:**
     - Session ID rotation hardening (explicit "New Session" button)
     - Offline/demo mode (mock backend for UI dev)
     - Session browser v2 (search, sort, soft-delete)
   - **Rationale:** These don't block v0.1.0 but are UX wins post-release

---

## Non-Blocking Items (GitHub Issues Suggested)

### Issue 1: Hook Test Coverage
**Title:** Add unit tests for useTauriEvents, useKeyboardShortcuts, useFocusTrap  
**Labels:** testing, a11y  
**Description:**
```
The three core app hooks have zero test coverage:
- useTauriEvents: 9 event subscriptions, cleanup on unmount
- useKeyboardShortcuts: 4 shortcuts + typing-context guard
- useFocusTrap: focus management for modal dialogs

Expected coverage:
- useTauriEvents: cleanup edge case (unmount during async setup)
- useKeyboardShortcuts: typing-context (INPUT, TEXTAREA, contenteditable)
- useFocusTrap: backward/forward focus cycling, opener unmounted edge case

Tests should use vitest + @testing-library/react. See apps/audio-graph/src/components/Toast.test.tsx for example.
```

### Issue 2: SettingsPage Refactoring
**Title:** Split SettingsPage monolith into 5 sub-components  
**Labels:** refactor, frontend  
**Description:**
```
Current SettingsPage is 1910 lines. Clear separation boundaries exist:
- AudioSettings (audio capture config)
- AsrProviderSettings (6 ASR providers + test connection)
- LlmProviderSettings (4 LLM providers + test connection)
- GeminiSettings (Gemini Live auth + model)
- CredentialsManager (save/delete workflow + AWS key mirroring)

Refactoring should:
- Keep useReducer + dispatch at parent level
- Pass handlers as props to sub-components
- Preserve AWS credential mirroring (shared secret key + token)
- Maintain i18n context (pass `t` prop)

Acceptance: Behavior identical to current, total line count reduced by ~50%, sub-components reusable.
```

### Issue 3: Session Management UX
**Title:** Enhance SessionsBrowser with search, sort, soft-delete  
**Labels:** ux, enhancement  
**Description:**
```
Current SessionsBrowser:
- No search (users with 100+ sessions can't find old ones)
- No sorting (sessions in creation order only)
- Hard delete (immediate, no undo)

Proposed v2:
- Real-time transcript search (client-side keyword grep)
- Sort by: date (desc), transcript length, top entities
- Soft-delete: 7-day trash folder, restore option, then hard-delete

Expected benefit: Improved UX for power users with long capture history.
```

---

## Conclusion

**A1 Readiness:** ✅ Architecture is sound. Invariants identified. Refactoring safe if handlers preserved at parent, AWS credential mirroring guarded, i18n passed as prop.

**A2 Readiness:** ❌ No tests in codebase. 3 core hooks are untested. Gaps: cleanup edge cases, typing-context guards, focus restoration. Priority for Loop 24.

**Post-v0.1.0:** Session rotation, offline mode, session browser v2 are valid enhancements but don't block release.

**No Regressions Found:** SettingsPage correctly handles complex credential lifecycle, event debouncing, test timeouts, and async cleanup. Ready for refactoring with diligence on the listed invariants.
