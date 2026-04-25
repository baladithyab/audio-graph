/**
 * Root React component for the AudioGraph Tauri window.
 *
 * Layout (desktop-first):
 *   - Top: `StorageBanner` (ENOSPC retry) + `DemoModeBanner` (first-launch
 *     local-only hint) + `ControlBar` (Start/Stop, settings, sessions).
 *   - Middle 3-column flex:
 *       - Left  aside: `AudioSourceSelector` + `SpeakerPanel`
 *       - Main:         `KnowledgeGraphViewer`
 *       - Right aside: `LiveTranscript` / `ChatSidebar` (tabbed) +
 *                      `TokenUsagePanel`
 *   - Bottom: `PipelineStatusBar` (per-stage status dots).
 *   - Overlays: error toast, `SettingsPage` modal, `SessionsBrowser` modal,
 *     `ShortcutsHelpModal`, first-launch `ExpressSetup` quickstart,
 *     `Toast` (transient status).
 *
 * Side-effects mounted at the root:
 *   - `useTauriEvents()` subscribes to all backend events exactly once.
 *   - `useKeyboardShortcuts()` registers global hotkeys (Cmd/Ctrl+R, Cmd/Ctrl+,
 *     Cmd/Ctrl+Shift+S, Escape).
 *   - A local `keydown` listener toggles the shortcuts help modal on
 *     Cmd/Ctrl+/ or "?" (outside of typing contexts).
 *
 * First-launch Express Setup is triggered from this component: on mount we
 * probe `credentials.yaml` via `load_credential_cmd` for any known cloud
 * provider key. If none exist, `ExpressSetup` renders once; dismissal is
 * transient (per-session), not persisted.
 *
 * No props — this component is the app shell.
 */
import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import AudioSourceSelector from "./components/AudioSourceSelector";
import LiveTranscript from "./components/LiveTranscript";
import ChatSidebar from "./components/ChatSidebar";
import KnowledgeGraphViewer from "./components/KnowledgeGraphViewer";
import ControlBar from "./components/ControlBar";
import SpeakerPanel from "./components/SpeakerPanel";
import PipelineStatusBar from "./components/PipelineStatusBar";
import SettingsPage from "./components/SettingsPage";
import SessionsBrowser from "./components/SessionsBrowser";
import ShortcutsHelpModal from "./components/ShortcutsHelpModal";
import ExpressSetup from "./components/ExpressSetup";
import TokenUsagePanel from "./components/TokenUsagePanel";
import Toast from "./components/Toast";
import StorageBanner from "./components/StorageBanner";
import DemoModeBanner from "./components/DemoModeBanner";
import { useTauriEvents } from "./hooks/useTauriEvents";
import { useKeyboardShortcuts } from "./hooks/useKeyboardShortcuts";
import { useAudioGraphStore } from "./store";
import "./App.css";

// Credential keys that, when any is present in credentials.yaml, indicate the
// user has already configured at least one provider. Missing all of these
// triggers the Express Setup quickstart on launch. Matches the cloud-provider
// keys the Express dialog writes to — local-only users fall through to Skip.
const FIRST_TIME_CREDENTIAL_KEYS = [
  "openai_api_key",
  "groq_api_key",
  "gemini_api_key",
  "deepgram_api_key",
  "assemblyai_api_key",
  "aws_access_key",
];

function App() {
  // Subscribe to Tauri backend events
  useTauriEvents();
  // Register global keyboard shortcuts (Cmd/Ctrl+R, Cmd/Ctrl+,, Esc, Cmd/Ctrl+Shift+S)
  useKeyboardShortcuts();

  const error = useAudioGraphStore((s) => s.error);
  const clearError = useAudioGraphStore((s) => s.clearError);
  const rightPanelTab = useAudioGraphStore((s) => s.rightPanelTab);
  const setRightPanelTab = useAudioGraphStore((s) => s.setRightPanelTab);
  const settingsOpen = useAudioGraphStore((s) => s.settingsOpen);
  const sessionsBrowserOpen = useAudioGraphStore((s) => s.sessionsBrowserOpen);
  const openSettings = useAudioGraphStore((s) => s.openSettings);

  // First-time setup: on mount, probe credentials.yaml for any known cloud
  // provider key. If none are present, pop the Express Setup modal once.
  // Dismissal (save or skip) sets `expressSetupVisible = false` and we never
  // re-probe during this session — the user can reach the same UI via
  // Settings when they're ready.
  const [expressSetupVisible, setExpressSetupVisible] = useState(false);
  useEffect(() => {
    let cancelled = false;
    (async () => {
      try {
        const results = await Promise.all(
          FIRST_TIME_CREDENTIAL_KEYS.map((key) =>
            invoke<string | null>("load_credential_cmd", { key }).catch(
              () => null,
            ),
          ),
        );
        if (cancelled) return;
        const hasAny = results.some((v) => v && v.length > 0);
        if (!hasAny) {
          setExpressSetupVisible(true);
        }
      } catch {
        // Silently tolerate probe failures — the user can still reach
        // Settings manually.
      }
    })();
    return () => {
      cancelled = true;
    };
  }, []);

  // Shortcuts help modal is kept as local UI state rather than in the store —
  // it has no backend tie-in and nothing else observes it.
  const [shortcutsOpen, setShortcutsOpen] = useState(false);
  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      // Cmd/Ctrl+/ (or Shift+/ → "?") opens the help modal. Skip when typing
      // into inputs so "?" remains typeable.
      const target = e.target as HTMLElement | null;
      const typing =
        !!target &&
        (target.tagName === "INPUT" ||
          target.tagName === "TEXTAREA" ||
          target.isContentEditable);
      if (typing) return;
      const mod = e.metaKey || e.ctrlKey;
      if (mod && e.key === "/") {
        e.preventDefault();
        setShortcutsOpen((open) => !open);
      } else if (!mod && e.key === "?") {
        e.preventDefault();
        setShortcutsOpen((open) => !open);
      }
    };
    window.addEventListener("keydown", handler);
    return () => window.removeEventListener("keydown", handler);
  }, []);

  return (
    <div className="app-container">
      <StorageBanner />
      <DemoModeBanner />
      <ControlBar />
      <div className="main-layout">
        <aside className="left-panel">
          <AudioSourceSelector />
          <SpeakerPanel />
        </aside>
        <main className="center-panel">
          <KnowledgeGraphViewer />
        </main>
        <aside className="right-panel">
          <div className="right-panel__tabs">
            <button
              className={`right-panel__tab ${rightPanelTab === "transcript" ? "right-panel__tab--active" : ""}`}
              onClick={() => setRightPanelTab("transcript")}
            >
              📝 Transcript
            </button>
            <button
              className={`right-panel__tab ${rightPanelTab === "chat" ? "right-panel__tab--active" : ""}`}
              onClick={() => setRightPanelTab("chat")}
            >
              💬 Chat
            </button>
          </div>
          {rightPanelTab === "transcript" ? (
            <LiveTranscript />
          ) : (
            <ChatSidebar />
          )}
          <TokenUsagePanel />
        </aside>
      </div>
      <PipelineStatusBar />

      {/* Error toast notification */}
      {error && (
        <div className="error-toast" role="alert">
          <span className="error-toast__icon" aria-hidden="true">
            ⚠️
          </span>
          <span className="error-toast__message">{error}</span>
          <button
            className="error-toast__dismiss"
            onClick={clearError}
            aria-label="Dismiss error"
          >
            ✕
          </button>
        </div>
      )}

      {/* Settings modal */}
      {settingsOpen && <SettingsPage />}

      {/* Sessions browser modal */}
      {sessionsBrowserOpen && <SessionsBrowser />}

      {/* Keyboard shortcuts help modal (Cmd/Ctrl+/ or ?) */}
      {shortcutsOpen && (
        <ShortcutsHelpModal onClose={() => setShortcutsOpen(false)} />
      )}

      {/* First-time quickstart — suppressed once Settings is open so the
          two modals don't stack. */}
      {expressSetupVisible && !settingsOpen && (
        <ExpressSetup
          onDismiss={() => setExpressSetupVisible(false)}
          onOpenAdvanced={() => openSettings()}
        />
      )}

      {/* Ephemeral status toast (Gemini reconnect, etc.) */}
      <Toast />
    </div>
  );
}

export default App;
