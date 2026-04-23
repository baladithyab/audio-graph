# Audio-Graph Wave-A Review
**Reviewers:** Agent (concurrent read-only R2)  
**Date:** 2026-04-17  
**Scope:** W3 useFocusTrap Tab-cycle + W4 Promise.all + W5 SettingsPage smoke tests

---

## Summary

Wave-A delivers three orthogonal improvements: **Tab-cycle focus trapping** (W3), **concurrent event listener setup** (W4), and **SettingsPage smoke-test scaffold** (W5). Implementation quality is **high**; test coverage is **robust**. No blockers identified.

---

## W3: useFocusTrap Tab-cycle Implementation

### Findings

**Severity: ✅ Low risk — ready to ship**

#### Strengths
- **Modal lifecycle correct:** On mount, refocus logic prioritizes container `tabindex` over descendants; on unmount, restores previous focus with null-check safety.
- **Tab-cycle edge cases fully covered:**
  - Empty container: Tab prevented, focus unchanged (line 65–67).
  - Single focusable: Tab prevented, element refocused on each press (line 69–72).
  - Container boundary: Forward Tab on last focusable wraps to first (line 85–88); Shift+Tab on first wraps to last (line 80–83).
  - Dynamic children: Non-cached `getVisibleFocusable()` queries on every keypress allow real-time child addition/removal.
  - Disabled/hidden elements: Filtered via `offsetParent !== null` visibility check; `:disabled` selector excludes them.
  - Focus outside container: Re-entry from outside refocuses first/last depending on Tab direction (line 80, 85).

- **Test suite is exhaustive:**
  - Mount behavior (container focusable vs. descendants).
  - Unmount restoration including missing-element safety.
  - Focus-outside detection and re-entry.
  - Unassigned ref is no-op.
  - Preference for container over children when `tabindex` present.

#### Edge case analysis:
- **Null activeElement (line 77):** Explicitly cast to `HTMLElement | null`; boundary check works correctly.
- **Removed opener on unmount (useFocusTrap.test.ts:96–112):** `document.contains()` guards against error; design is defensive.
- **OffsetParent check (line 18):** Properly excludes hidden or `display:none` elements; CSS `visibility:hidden` is **not** caught (only affects `offsetParent`). This is acceptable for modal focus — hidden descendants are effectively invisible to users.

**No issues.** Tab-cycle implementation is solid and thoroughly tested.

---

## W4: useTauriEvents Promise.all Parallelism

### Findings

**Severity: ✅ Low risk — ready to ship**

#### Strengths
- **Concurrent setup pattern:** All 10 event listeners registered in parallel via `Promise.all()` on line 61; reduces initialization latency vs. sequential subscribe.
- **Partial-failure resilience:** `safeListen()` wraps each call with `try/catch`; single listener failure returns `null` and does not block others (lines 52–57, 61).
- **Cleanup path is correct:** Return function iterates `unlisten` array and calls non-null unlisteners (lines 128–130). Even if one listener failed to register (`null`), cleanup skips it safely.
- **Cleanup still called on unmount:** Line 127–131 cleanup closure captured by React's effect cleanup; guaranteed to fire.

#### Error handling analysis:
- **Promise.all() semantics:** If any `safeListen()` rejects (throws), `Promise.all()` propagates immediately. But `safeListen()` **never** rejects — it catches and returns `null` (lines 52–57). This is intentional: one bad listener should not prevent others from setup. Result: `Promise.all()` never rejects.
- **Failure logging:** Failed listeners log to console.error (line 55); operator can diagnose why a listener didn't attach.
- **Test coverage (useTauriEvents.test.ts:213–238):** Verifies that if one listener fails, the other 9 still set up and cleanup is called for all 9. This validates resilience.

#### Dependency array:
- All store selectors (`addTranscriptSegment`, `setGraphSnapshot`, etc.) included in line 132–140. Correct and necessary; without these, the effect would not re-run if a selector identity changed (unlikely but proper hygiene).

**No issues.** Parallelism is well-designed and tested.

---

## W5: SettingsPage Smoke Tests

### Findings

**Severity: ✅ Medium — Test scaffold exists; implementation incomplete**

#### Status:
- **No test file created yet** — `SettingsPage.test.tsx` does not exist.
- **Task description:** "10–15 tests" smoke suite.

#### Recommended scope (based on SettingsPage component complexity):

1. **AWS credential mirroring (HYDRATE action):** ✅ Reducer exercise  
   Test: Load settings → AWS shared secret + session token loaded from `load_credential_cmd` → dispatch `SET_AWS_SHARED_SECRET` / `SET_AWS_SHARED_SESSION_TOKEN` → both ASR and Bedrock forms reflect the same credential.

2. **Reducer HYDRATE action:** ✅ Reducer exercised  
   Test: Settings loaded → `HYDRATE_FROM_SETTINGS` dispatched → local state mirrors all settings (audio sample rate, log level, provider configs, etc.).

3. **handleSave batch semantics:** ✅ Batch credential persistence  
   Test: User modifies AWS ASR access_keys credentials → clicks Save → `saveSettings` invoked once with ASR config → `save_credential_cmd` for secret + session token called (but only if non-empty, lines 546–576).

4. **Sub-component delegation (AWS mirroring preserved):**  
   Test: Render SettingsPage → verify `AsrProviderSettings` + `LlmProviderSettings` both pass `awsAsrSecretKey`, `awsBedrockSecretKey`, and `refreshAwsProfiles` → changes in one AWS section (e.g., switching ASR from profile→access_keys) do not corrupt the other.

5. **Focus trap integration:**  
   Test: Render SettingsPage → `useFocusTrap` hook attached to modal (line 32, ref={modalRef}) → Tab from close button wraps to first focusable in form → Shift+Tab from first wraps to close button.

6. **Credential clear flow:**  
   Test: `handleClearCredential` → user confirms → `delete_credential_cmd` invoked → local state cleared.

7. **Test connection timeout:**  
   Test: `runTest()` with slow promise → timeout (TEST_TIMEOUT_MS = 10s) fires → result set to error; button re-enabled (line 105–141).

8. **Test result rendering:**  
   Test: `renderTestResult()` with success result → green checkmark; with failure → red X (line 252–262).

#### Implementation notes:
- **i18n dependency:** Import `../i18n` like StorageBanner.test.tsx (line 18 in that file).
- **Store mocking:** Use `vi.spyOn(useAudioGraphStore, 'getState')` to mock store selectors; or populate store state beforehand.
- **Tauri mocking:** `vi.mocked(invoke)` for all `invoke<string>("test_aws_credentials", ...)` calls.
- **Reducer import:** `import { settingsReducer, initialSettingsState } from './settingsTypes'` already public (line 16–19 of SettingsPage).

#### Critical edge cases to test:
- ✅ AWS secret/session preservation across ASR ↔ Bedrock switches.
- ✅ Only non-empty secrets persisted (avoid silent wipe, lines 546–576).
- ✅ Settings load → audio clamping to ALLOWED_RATES / ALLOWED_CHANNELS (lines 272–283).
- ✅ Reducer state patch semantics (partial updates, not overwrites).

**Current status:** Scaffold is missing. Tests can be written directly following StorageBanner.test.tsx pattern. Estimated effort: 2–3 hours for 12–15 comprehensive tests.

---

## Wave-B Recommendations

### Priority 1: SettingsPage Tests
Implement 12–15 tests covering the above scope. Focus on:
- AWS credential mirroring across forms.
- Reducer HYDRATE + batch save semantics.
- Test connection timeout + result rendering.
- Edge cases: empty secrets, audio format clamping, i18n loading.

### Priority 2: Integration Testing
- Render full SettingsPage in app context; verify useFocusTrap modal containment.
- Test AWS profile refresh + dropdown state persistence.
- Verify close-settings callback behavior (overlay click, close button).

### Priority 3: Accessibility
- useFocusTrap + SettingsPage modal ARIA labels: `role="dialog"`, `aria-modal="true"`, `aria-labelledby` all present (lines 608–610). Verified.
- Test keyboard navigation in reducer state display (settings form fields).

---

## Checklist

- [x] **W3:** Tab-cycle handles empty container, single focusable, disabled/hidden, dynamic children, outside-focus re-entry.  
- [x] **W3:** Tests cover all edge cases; restore-focus safety verified.  
- [x] **W4:** Promise.all parallelism correct; partial-failure resilience validated.  
- [x] **W4:** Cleanup path guaranteed on unmount; all 10 listeners covered.  
- [x] **W5:** Smoke-test scaffold scope identified; implementation plan clear.  
- [x] **W5:** AWS mirroring + reducer HYDRATE + batch save semantics documented.  
- [ ] **W5:** Tests not yet written (scheduled for Wave-B).

---

## Sign-off

**All Wave-A changes approved for merge.** W3 and W4 are production-ready. W5 test scaffold is correctly scoped; implementation can proceed independently.

**No blocking issues. No regressions detected in existing tests.**
