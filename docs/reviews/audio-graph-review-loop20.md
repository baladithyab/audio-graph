# AudioGraph Loop 20 Code Review

**Review Date:** 2026-04-17  
**Reviewer:** B2 (Read-only audit)

## Focus Areas & Findings

### 1. **A4: Session Rotation Race Conditions** ⚠️ MAJOR

**File:** `src-tauri/src/commands.rs` (line 1818–1844) — `new_session_cmd()`

**Issue — No In-Process Rotation:**
The `new_session_cmd` does NOT rotate the `AppState::session_id` field in place. Instead:
- It finalizes the *current* session metadata
- It seeds a fresh usage file for the *next* session (with a new UUID)
- **But** the running transcript writer and graph autosave threads continue writing under the original `session_id` until process restart

**Race Condition Risk:**
```rust
// new_session_cmd returns a fresh UUID to the frontend
let new_id = uuid::Uuid::new_v4().to_string();  // ← returns this
// But AppState.session_id is never mutated
// The TX writer thread keeps appending to OLD session_id in transcript JSONL
```

**Impact:**
- Frontend receives `new_id` and displays "new session started"
- User believes capture is under the new session
- Capture actually still writes to the old `session_id`
- **Torn reads if frontend queries `get_session_id` mid-rotation:** returns old UUID (correct), new usage file was created (new), but TX keeps writing old (old). Inconsistent state.

**Deferred to Loop 21:**
The code comment (line 1812–1817) explicitly states this is deferred. A true rotation requires respawning the TX writer and autosave threads, which is scope for a later loop.

**Verdict:** Not a bug in the current code, but design is intentionally incomplete. Frontend developers must be aware `new_session_cmd` is a *metadata boundary* only; it does NOT guarantee a clean in-process split.

---

### 2. **A1: TokenUsagePanel Backend Hydration** ⚠️ GRACEFUL

**Files:**
- Frontend: `src/components/TokenUsagePanel.tsx` (lines 100–102)
- Backend: `src-tauri/src/commands.rs` (lines 1796–1799) — `get_current_session_usage()`

**First-Run Scenario (Session File Missing):**

1. **App Launch → new UUID generated** (`AppState.new()`)
2. **No session file exists yet** (`~/.audiograph/usage/<uuid>.json`)
3. **Frontend calls `loadTotals(SESSION_KEY)`** (localStorage lookup)
4. **Backend queried later for `get_current_session_usage`**
5. **Backend calls `sessions::usage::load_usage()`** (line 1799)
6. **`load_usage()` returns zeroed `SessionUsage` on missing file** (usage.rs:95–102)

**Result:** ✅ Graceful fallback
- Frontend gets `ZERO_TOTALS` from localStorage
- Backend gets zeroed `SessionUsage`
- Both render "empty" state, no crash
- **Implicit assumption:** `localStorage` key exists; if cleared, frontend state is lost but backend survives

**localStorage Merge Behavior:**
The current code **discards** old localStorage on new session:
```tsx
const [session, setSession] = useState<Totals>(() => loadTotals(SESSION_KEY));  // Reads old
// If SESSION_KEY was populated from prior session, it's kept. If cleared, starts at zero.
```

**No explicit merge logic.** Migration path for existing users:
- **Scenario A (Happy):** User restarts app → frontend rehydrates from old localStorage keys (`tokens.session.v1`, `tokens.lifetime.v1`)
- **Scenario B (Lossy):** User clears browser storage (Settings → Clear Cache) → localStorage wiped → backend still has files → user sees zero in UI but backend has history
- **Scenario C (Conflict):** User manually edits localStorage → possible stale data in UI, but backend usage file overwrites via `append_turn`

**Verdict:** Acceptable for loop 20, but document the merge semantics in a follow-up loop. No fallback needed; it's working as designed.

---

### 3. **A2: StorageBanner & Toast Coexistence** ❌ NOT FOUND

**Finding:** No `StorageBanner` component exists in the codebase.

**File Search Results:**
- `grep -r "StorageBanner\|ENOSPC\|disk.*full"` returned no matches in `src/components/*.tsx`
- `Toast.tsx` exists (lines 1–68) but has **no StorageBanner variant**
- `App.tsx` does not mount a StorageBanner component

**Current Toast Implementation:**
- Single toast at a time (module-level `listeners` Set)
- Auto-dismisses after 3.5 seconds
- Variants: `"success" | "info"` (no error/warning variant for disk-full alerts)
- **Screen real estate:** Renders in a single fixed slot; no stacking

**Implications for Loop 20:**
- **A2 (StorageBanner) may not be implemented yet** — or it's under a different name
- If it's meant to be a disk-full warning, the current `Toast` system can only show one notification at a time
- **Conflict risk if both Toast and error-toast try to render:** The `App.tsx` error-toast (lines 144–158) uses `store.error` and is independent from the module-level `Toast`, so they **could stack** if both trigger

**Verdict:** No StorageBanner found. Recommend confirming scope with A2 owner.

---

## Summary & Recommendations

### Top 3 for Loop 21

1. **A4: Implement in-process session rotation**
   - Mutate `AppState::session_id` under a guard (e.g., `Arc<RwLock<String>>`)
   - Respawn transcript writer + autosave threads with new session ID
   - Implement graceful drain: finish in-flight writes before thread respawn
   - Test torn reads: ensure `get_session_id()` never returns stale UUID

2. **A1: Document localStorage ↔ backend sync semantics**
   - Clarify: localStorage is frontend-only cache; backend file is source-of-truth
   - Define merge behavior for users upgrading from pre-loop19 installations
   - Add explicit localStorage-clear path in UI (not just browser DevTools)

3. **A2: Confirm StorageBanner scope & Toast stacking**
   - Locate or implement StorageBanner component
   - Design notification stacking if disk-full + other alerts fire simultaneously
   - Add error/warning variants to Toast (currently only success/info)

---

## Files Reviewed

- ✅ `src/components/TokenUsagePanel.tsx` — localStorage sync, Gemini event listener
- ✅ `src/components/Toast.tsx` — notification system
- ✅ `src/App.tsx` — component hierarchy, error-toast overlay
- ✅ `src/store/index.ts` — state management (no session_id rotation logic)
- ✅ `src-tauri/src/commands.rs` — `new_session_cmd`, `get_current_session_usage`
- ✅ `src-tauri/src/sessions/mod.rs` — session index + finalization
- ✅ `src-tauri/src/sessions/usage.rs` — disk-based usage persistence, error handling
- ✅ `src-tauri/src/state.rs` — `AppState` struct, no RwLock on session_id

## Verdict

**Loop 20 status:** Ready for merge with noted limitations on session rotation. No critical issues blocking review, but confirm A2 scope and define A4's thread respawn contract before loop 21 implementation.
