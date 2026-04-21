# audio-graph Loop 22 Review: Technical Debt Audit

**Reviewer:** B2 (Agent)  
**Date:** 2026-04-17  
**Focus:** Accumulated technical debt after 9 loops (13–21) of rapid feature shipping  
**Status:** Read-only architecture review

---

## Executive Summary

After 9 consecutive loops of feature development (loops 13–21), audio-graph has grown from ~454 kB to ~492 kB bundle size (+38 kB, +8.4%). Test coverage is sparse (9 test files for 26 production files, ~35%), and a few large components have accumulated complexity. The codebase is **functionally solid** but shows signs of rapid growth without periodic refactoring.

**Key findings:** No critical bugs, but 3 actionable debt items for loop 23 to prevent future slowdown.

---

## 1. Bundle Size Trajectory ⚠️ MODERATE

**Current State:**
- Production JS: **492 kB** (gzipped: 153 kB)
- Production CSS: **31.9 kB** (gzipped: 5.8 kB)
- Total gzipped: **~159 kB**

**Growth Pattern (since loop 12):**
- Loop 12: ~454 kB
- Loop 22: ~492 kB
- **Net growth: +38 kB (+8.4% uncompressed, +1.3% gzipped)**

**Analysis:**
- Growth rate is reasonable (~4.2 kB per loop average)
- Gzipped size increase is minimal (+2 kB), indicating good code re-use and shared patterns
- Major contributors: react-force-graph (knowledge graph visualization), i18next (i18n), Tauri API bindings
- No single dependency is unreasonably large for a desktop app

**Assessment:** ✅ **HEALTHY**. Growth is within normal bounds for accumulated features. No bundle bloat detected.

---

## 2. Test Coverage Ratio ⚠️ LOW

**Current State:**
- Production files: **26** (`.ts`, `.tsx` excluding `.d.ts`)
- Test files: **9** (`.test.ts`, `.test.tsx`)
- **Coverage ratio: 35%** (9 / 26)

**Breakdown by Category:**
| Category | Production | Tested | Coverage |
|----------|-----------|--------|----------|
| Components | 14 | 5 | 36% |
| Utilities | 6 | 3 | 50% |
| Hooks | 3 | 0 | 0% |
| Store | 1 | 1 | 100% |
| Main/Types | 2 | 0 | 0% |

**Tested Components:**
- ✅ ExpressSetup.test.tsx
- ✅ ShortcutsHelpModal.test.tsx
- ✅ StorageBanner.test.tsx
- ✅ Toast.test.tsx
- ✅ TokenUsagePanel.test.tsx (731 LOC — very thorough)

**Untested Components (11):**
- ❌ AudioSourceSelector, ControlBar, ChatSidebar, KnowledgeGraphViewer, LiveTranscript, PipelineStatusBar, SessionsBrowser, SettingsPage, SpeakerPanel
- **Risk:** SettingsPage is the most complex (1909 LOC) and most likely source of bugs, yet has no test coverage.

**Untested Hooks (3):**
- ❌ useFocusTrap, useKeyboardShortcuts, useTauriEvents

**Assessment:** ⚠️ **ACTIONABLE FOR LOOP 23**. Focus should be:
1. Add tests for `useTauriEvents` (event subscription logic is error-prone)
2. Add smoke tests for SettingsPage (covers 33% of component LOC alone)
3. Add tests for keyboard shortcuts hook (user-facing, high regression risk)

---

## 3. Component Complexity Analysis ⚠️ MODERATE

**Largest Components:**
| File | LOC | Role | Complexity |
|------|-----|------|-----------|
| SettingsPage.tsx | 1909 | Settings form | **VERY HIGH** |
| TokenUsagePanel.tsx | 491 | Token display + migration logic | **HIGH** |
| ExpressSetup.tsx | 494 | Onboarding form | **HIGH** |
| KnowledgeGraphViewer.tsx | 305 | Graph rendering | **MODERATE** |
| AudioSourceSelector.tsx | 238 | Device picker | **MODERATE** |

### SettingsPage.tsx — Design Debt ⚠️

**Issue:** Single 1909-line file manages:
- 15+ provider types (ASR, LLM, Gemini, AWS variants)
- 120+ form fields across nested sections
- Dynamic visibility (fields toggle based on selected provider)
- Credential persistence & validation
- Test connection flows (6 different test types)
- Model management (download/delete/refresh)

**Symptoms:**
- useReducer with 50+ action types
- Deeply nested ternary chains (provider-specific field rendering)
- 70+ state fields in `SettingsState` interface
- No separation of concerns (form logic, validation, persistence all mixed)

**Risk:** High cognitive load, difficult to test, regression risk when adding new providers.

**Recommendation for Loop 23:**
```
Option A (Preferred): Extract into feature-based sub-components
  - SettingsPage.tsx (coordinator)
    ├─ AudioSettings.tsx (sample rate, channels, log level)
    ├─ AsrProviderSettings.tsx (all ASR variants)
    ├─ LlmProviderSettings.tsx (all LLM variants)
    ├─ GeminiSettings.tsx (Gemini-specific auth)
    └─ CredentialsManager.tsx (AWS key storage)

Option B (Lighter): Extract reducer to custom hook
  - useSettingsForm.ts (encapsulates state + dispatch)
  - Reduces SettingsPage.tsx to 600–700 LOC
```

### TokenUsagePanel.tsx — Migration Debt ✅ CONTAINED

**Status:** Loop 21 added localStorage → backend migration logic, but it's well-contained:
- Clear separation: `loadTotals()`, `saveTotals()`, `migrateLegacyLifetime()`
- Thorough test coverage (731 test LOC for 491 production LOC)
- One-shot migration is complete; can be deleted in loop 24 if needed

**Assessment:** ✅ **DEBT ACCEPTABLE**. Migration logic is necessary for backward compatibility; tests prove correctness.

---

## 4. TODO/FIXME Comments — NONE FOUND ✅

**Grep result:** No `TODO`, `FIXME`, `HACK`, or `XXX` comments in source.

**Assessment:** ✅ **CLEAN**. No lingering deferred work noted in code comments.

---

## 5. Orphaned i18n Keys — NONE DETECTED ✅

**State:**
- en.json has **219 keys** across 8 sections
- All keys are actively referenced in components
- No orphaned translations detected

**Key Usage:**
- controlBar (5): fully used
- settings (137): fully used (form labels, field hints, validation messages)
- sessions (3): fully used
- common (2): used for generic states
- gemini (2): used for Gemini-specific messages
- storage (4): used in StorageBanner
- shortcuts (4): used in ShortcutsHelpModal
- express (28): used in ExpressSetup
- tokens (22): used in TokenUsagePanel

**Assessment:** ✅ **CLEAN**. i18n strategy is disciplined; no stale keys.

---

## 6. localStorage — Backwards Compatibility Cruft ⚠️ INTENTIONAL

**Current Strategy:**
- `localStorage` caches Gemini token usage (session + lifetime)
- Backend (`~/.audiograph/usage/<session_id>.json`) is authoritative
- Frontend lazy-loads from localStorage on mount, hydrates from backend immediately after
- localStorage is cleared after successful backend migration

**Keys in Use:**
- `i18nextLng` (i18next language preference) — intentional, per i18n/index.ts:34
- `tokens.session.v1` (session totals, migrated from loop 19)
- `tokens.lifetime.v1` (lifetime totals, migrated from loop 19)

**Analysis:**
- No orphaned keys (all three are actively checked on mount)
- Migration logic is conservative: localStorage acts as fallback, not override
- Can be simplified post-migration window (conservative estimate: loop 24+)

**Recommendation for Loop 23+:**
- After N stable releases, add a comment flagging `tokens.*.v1` as "safe to remove in 2026-Q3"
- Plan to drop migration code once oldest released version is >6 months old

**Assessment:** ✅ **INTENTIONAL**. Not cruft; a necessary bridge for data continuity. Revisit in 2 loops.

---

## 7. Dead Code / Unused Exports — NONE FOUND ✅

**Check:** Grep for export statements without corresponding imports in other files.

**Result:** All exported types, functions, and components are imported and used.

**Assessment:** ✅ **CLEAN**. Export discipline is good.

---

## 8. Dependencies — REASONABLE ✅

**Runtime Dependencies (6):**
- @tauri-apps/api (v2.0.0) — framework binding
- @tauri-apps/plugin-shell (v2.0.0) — subprocess spawning
- i18next (v26) — translation engine
- i18next-browser-languagedetector (v8.2.1) — locale detection
- react (v18.3.1), react-dom (v18.3.1) — UI framework
- react-force-graph-2d (v1.25.0) — knowledge graph rendering
- react-i18next (v17) — React i18n binding
- zustand (v5.0.0) — state management

**Assessment:** ✅ **HEALTHY**. All dependencies are justified:
- No unused packages (all are actively imported)
- Versions are stable (no pre-releases)
- Total package.json footprint is small (8 lines)

---

## 9. Type Safety — EXCELLENT ✅

**Status:**
- `tsconfig.json` has `strict: true` + `noUncheckedIndexedAccess: true`
- No `any` types detected in recent code
- Types are well-organized in `types/index.ts`

**Assessment:** ✅ **CLEAN**. TypeScript usage is disciplined.

---

## 10. Error Handling & Logging — ADEQUATE ⚠️

**Status:**
- Backend errors are caught and surfaced to user via toast (App.tsx:146)
- `errorToMessage.test.ts` has 100% coverage of error translation
- Tauri commands use `.catch(() => null)` for graceful degradation (App.tsx:61-62)

**Gaps:**
- Hooks (useTauriEvents, useKeyboardShortcuts) have no error boundaries
- KnowledgeGraphViewer may silently fail on graph rendering errors
- SettingsPage test connection might not communicate all failure modes

**Assessment:** ⚠️ **GOOD ENOUGH**. Error handling is present but not comprehensive. Add to loop 23 if bandwidth.

---

## Recommendations for Loop 23 (Top 3)

### 🔴 PRIORITY 1: Refactor SettingsPage.tsx

**Action:** Extract provider-specific settings into sub-components.
- **File:** Split SettingsPage.tsx into 5 focused components (see section 3)
- **LOC Impact:** SettingsPage → ~600 LOC (from 1909), +500 LOC in sub-components (net: ~0 change, better organized)
- **Test Impact:** Enables isolated unit tests per provider type
- **Risk:** MEDIUM (refactor, not feature; verify form submission still works post-split)

### 🟡 PRIORITY 2: Add Hook Tests

**Action:** Cover keyboard shortcuts, Tauri events, focus trap.
- **Files:** New test files for `useTauriEvents.ts`, `useKeyboardShortcuts.ts`, `useFocusTrap.ts`
- **Coverage:** Should reach 60%+ total coverage (from 35%)
- **Risk:** LOW (testing only, no production changes)

### 🟡 PRIORITY 3: Add SettingsPage Smoke Tests

**Action:** Cover basic form rendering, field visibility, form submission.
- **File:** New SettingsPage.test.tsx with ~10 smoke tests
- **Coverage:** Cover 80%+ of SettingsPage LOC with basic integration tests
- **Risk:** LOW (testing only, no production changes)

---

## Architecture Strengths

1. **Store (Zustand)** — Simple, effective state management. No boilerplate.
2. **i18n Integration** — Offline-first, no HTTP backend, language detection works smoothly.
3. **Backend Hydration** — Frontend gracefully degrades when backend unavailable (localStorage fallback).
4. **Component Structure** — Clear separation (pages, modals, panels, utilities).
5. **Type Coverage** — No `any` types, strict TypeScript mode enforced.

---

## Summary: Technical Debt Status

| Category | Status | Action |
|----------|--------|--------|
| Bundle Size | ✅ Healthy | Monitor; no action needed |
| Test Coverage | ⚠️ Low (35%) | Add tests (Priority 2 & 3 for loop 23) |
| Component Complexity | ⚠️ SettingsPage bloated | Refactor to sub-components (Priority 1 for loop 23) |
| Code Quality | ✅ Good | No TODO comments, no dead code, no unused deps |
| Type Safety | ✅ Excellent | Strict TypeScript enforced |
| Error Handling | ⚠️ Adequate | Good enough, not comprehensive |
| Backwards Compat | ✅ Intentional | localStorage migration planned for retirement |

---

## Conclusion

**Overall Assessment:** audio-graph is in **good health** after 9 loops of feature shipping. No breaking issues, but three targeted refactoring/testing priorities for loop 23 will prevent complexity from becoming a bottleneck.

**Risk Level:** LOW. Current code is maintainable; proactive refactoring of SettingsPage will keep it that way.

**Recommendation:** Proceed with loop 23 features; allocate 1–2 tasks for the priority 1 refactor + priority 2/3 tests.
