/**
 * Global frontend store (Zustand) — the single source of truth for UI
 * state plus the invoke-bridge to the Rust backend.
 *
 * Slice layout:
 *   - Audio sources          — `audioSources`, `selectedSourceIds`,
 *                              `searchFilter`, `processes` + fetchers.
 *   - Capture lifecycle      — `isCapturing`, `captureStartTime`,
 *                              `startCapture` / `stopCapture` (wrap
 *                              `start_capture` / `stop_capture`).
 *   - Transcribe pipeline    — `isTranscribing`, `startTranscribe` /
 *                              `stopTranscribe` + the live
 *                              `transcriptSegments` buffer populated by
 *                              `TRANSCRIPT_UPDATE` events.
 *   - Gemini Live            — `isGeminiActive`, `startGemini` /
 *                              `stopGemini`, plus the separate
 *                              `geminiTranscripts` buffer appended by
 *                              `GEMINI_TRANSCRIPTION` events.
 *   - Knowledge graph        — `graphSnapshot` (refreshed on
 *                              `GRAPH_UPDATE`) + the `exportGraph` and
 *                              `getSessionId` command wrappers.
 *   - Speakers               — `speakers` (upserted on
 *                              `SPEAKER_DETECTED` events).
 *   - Pipeline status        — `pipelineStatus` + per-source
 *                              `backpressuredSources` set.
 *   - Chat                   — `chatMessages`, `isChatLoading`,
 *                              `sendChatMessage`, `clearChatHistory`.
 *   - Settings / UI          — `settings`, `loadSettings`,
 *                              `settingsOpen` / `sessionsBrowserOpen`
 *                              modal flags + `rightPanelTab` tab state.
 *   - Error + toast wiring   — `error`, `setError`, `clearError`.
 *
 * The invoke-bridge contract: each async action that touches Rust
 * wraps `invoke<T>(command, args)` and translates thrown errors via
 * `utils/errorToMessage`. Events flow the other way — `useTauriEvents`
 * mutates this store on every backend event. See that hook for the
 * full list of subscriptions.
 *
 * Unit tests that exercise slices pull `useAudioGraphStore.getState()`
 * directly and `setState` to seed fixtures.
 */
import { create } from "zustand";
import { invoke } from "@tauri-apps/api/core";
import type {
    ApiEndpointConfig,
    AppSettings,
    AudioGraphStore,
    AudioSourceInfo,
    ChatMessage,
    ChatResponse,
    GeminiTranscriptEntry,
    ModelInfo,
    ModelStatus,
    ProcessInfo,
    SessionMetadata,
    StageStatus,
    TranscriptSegment,
} from "../types";

const idleStage: StageStatus = { type: "Idle" };

export const useAudioGraphStore = create<AudioGraphStore>((set, get) => ({
    // ── Audio sources ────────────────────────────────────────────────────
    audioSources: [],
    selectedSourceIds: [],
    setAudioSources: (sources) => set({ audioSources: sources }),
    toggleSourceId: (id) =>
        set((state) => {
            const idx = state.selectedSourceIds.indexOf(id);
            if (idx >= 0) {
                return { selectedSourceIds: state.selectedSourceIds.filter((sid) => sid !== id) };
            }
            return { selectedSourceIds: [...state.selectedSourceIds, id] };
        }),
    clearSelectedSources: () => set({ selectedSourceIds: [] }),
    fetchSources: async () => {
        try {
            const sources = await invoke<AudioSourceInfo[]>("list_audio_sources");
            set({ audioSources: sources, error: null });
        } catch (e) {
            set({ error: e instanceof Error ? e.message : String(e) });
        }
    },

    // ── Processes ────────────────────────────────────────────────────────
    processes: [],
    searchFilter: '',
    fetchProcesses: async () => {
        try {
            const processes = await invoke<ProcessInfo[]>("list_running_processes");
            set({ processes });
        } catch (err) {
            console.error("Failed to fetch processes:", err);
        }
    },
    setSearchFilter: (filter: string) => set({ searchFilter: filter }),

    // ── Transcript ───────────────────────────────────────────────────────
    transcriptSegments: [],
    addTranscriptSegment: (segment) =>
        set((state) => ({
            transcriptSegments: [...state.transcriptSegments.slice(-499), segment],
        })),
    clearTranscript: () => set({ transcriptSegments: [] }),

    // ── Knowledge graph ──────────────────────────────────────────────────
    graphSnapshot: {
        nodes: [],
        links: [],
        stats: { total_nodes: 0, total_edges: 0, total_episodes: 0 },
    },
    setGraphSnapshot: (snapshot) => set({ graphSnapshot: snapshot }),

    // ── Exports ──────────────────────────────────────────────────────────
    exportTranscript: async () => {
        return await invoke<string>("export_transcript");
    },
    exportGraph: async () => {
        return await invoke<string>("export_graph");
    },
    getSessionId: async () => {
        return await invoke<string>("get_session_id");
    },

    // ── Pipeline status ──────────────────────────────────────────────────
    pipelineStatus: {
        capture: idleStage,
        pipeline: idleStage,
        asr: idleStage,
        diarization: idleStage,
        entity_extraction: idleStage,
        graph: idleStage,
    },
    setPipelineStatus: (status) => set({ pipelineStatus: status }),

    // ── Speakers ─────────────────────────────────────────────────────────
    speakers: [],
    addOrUpdateSpeaker: (speaker) =>
        set((state) => {
            const idx = state.speakers.findIndex((s) => s.id === speaker.id);
            if (idx >= 0) {
                const updated = [...state.speakers];
                updated[idx] = speaker;
                return { speakers: updated };
            }
            return { speakers: [...state.speakers, speaker] };
        }),
    clearSpeakers: () => set({ speakers: [] }),

    // ── Capture state ────────────────────────────────────────────────────
    isCapturing: false,
    captureStartTime: null,
    setIsCapturing: (capturing) => set({ isCapturing: capturing }),
    backpressuredSources: [],
    setSourceBackpressure: (sourceId, isBackpressured) =>
        set((state) => {
            const present = state.backpressuredSources.includes(sourceId);
            if (isBackpressured && !present) {
                return { backpressuredSources: [...state.backpressuredSources, sourceId] };
            }
            if (!isBackpressured && present) {
                return {
                    backpressuredSources: state.backpressuredSources.filter(
                        (id) => id !== sourceId,
                    ),
                };
            }
            return {};
        }),
    startCapture: async () => {
        const { selectedSourceIds } = get();
        if (selectedSourceIds.length === 0) {
            set({ error: "No audio source selected" });
            return;
        }
        try {
            for (const sourceId of selectedSourceIds) {
                await invoke("start_capture", { sourceId });
            }
            set({
                isCapturing: true,
                captureStartTime: Date.now(),
                error: null,
            });
        } catch (e) {
            set({ error: e instanceof Error ? e.message : String(e) });
        }
    },
    stopCapture: async () => {
        const { selectedSourceIds } = get();
        if (selectedSourceIds.length === 0) return;
        try {
            for (const sourceId of selectedSourceIds) {
                await invoke("stop_capture", { sourceId });
            }
            set({
                isCapturing: false,
                isTranscribing: false,
                isGeminiActive: false,
                captureStartTime: null,
                backpressuredSources: [],
                error: null,
            });
        } catch (e) {
            set({ error: e instanceof Error ? e.message : String(e) });
        }
    },

    // ── Transcribe state ────────────────────────────────────────────────────────
    isTranscribing: false,
    startTranscribe: async () => {
        const { isCapturing } = get();
        if (!isCapturing) {
            set({ error: "Cannot start transcription: capture is not running" });
            return;
        }
        try {
            await invoke("start_transcribe");
            set({
                isTranscribing: true,
                error: null,
            });
        } catch (e) {
            set({ error: e instanceof Error ? e.message : String(e) });
        }
    },
    stopTranscribe: async () => {
        try {
            await invoke("stop_transcribe");
            set({
                isTranscribing: false,
                error: null,
            });
        } catch (e) {
            set({ error: e instanceof Error ? e.message : String(e) });
        }
    },

    // ── Gemini Live dual pipeline ─────────────────────────────────────────
    isGeminiActive: false,
    geminiTranscripts: [],
    addGeminiTranscript: (entry: GeminiTranscriptEntry) =>
        set((state) => ({
            geminiTranscripts: [...state.geminiTranscripts.slice(-499), entry],
        })),
    clearGeminiTranscripts: () => set({ geminiTranscripts: [] }),
    startGemini: async () => {
        const { isCapturing } = get();
        if (!isCapturing) {
            set({ error: "Cannot start Gemini: capture is not running" });
            return;
        }
        try {
            await invoke("start_gemini");
            set({
                isGeminiActive: true,
                error: null,
            });
        } catch (e) {
            set({ error: e instanceof Error ? e.message : String(e) });
        }
    },
    stopGemini: async () => {
        try {
            await invoke("stop_gemini");
            set({
                isGeminiActive: false,
                error: null,
            });
        } catch (e) {
            set({ error: e instanceof Error ? e.message : String(e) });
        }
    },

    // ── Error state ──────────────────────────────────────────────────────
    error: null,
    setError: (error) => set({ error }),
    clearError: () => set({ error: null }),

    // ── Chat ─────────────────────────────────────────────────────────────
    chatMessages: [],
    isChatLoading: false,
    rightPanelTab: "transcript",
    setRightPanelTab: (tab) => set({ rightPanelTab: tab }),
    sendChatMessage: async (message: string) => {
        // Add user message immediately for responsiveness
        const userMsg: ChatMessage = { role: "user", content: message };
        set((state) => ({
            chatMessages: [...state.chatMessages, userMsg],
            isChatLoading: true,
        }));

        try {
            const response = await invoke<ChatResponse>("send_chat_message", { message });
            set((state) => ({
                chatMessages: [...state.chatMessages, response.message],
                isChatLoading: false,
            }));
        } catch (e) {
            // Add error as assistant message
            const errorMsg: ChatMessage = {
                role: "assistant",
                content: `Error: ${e instanceof Error ? e.message : String(e)}`,
            };
            set((state) => ({
                chatMessages: [...state.chatMessages, errorMsg],
                isChatLoading: false,
            }));
        }
    },
    clearChatHistory: async () => {
        try {
            await invoke("clear_chat_history");
            set({ chatMessages: [] });
        } catch (e) {
            set({ error: e instanceof Error ? e.message : String(e) });
        }
    },

    // ── Models ────────────────────────────────────────────────────────────
    models: [],
    isDownloading: false,
    downloadProgress: null,
    fetchModels: async () => {
        try {
            const models = await invoke<ModelInfo[]>("list_available_models");
            set({ models, error: null });
        } catch (e) {
            set({ error: e instanceof Error ? e.message : String(e) });
        }
    },
    downloadModel: async (filename: string) => {
        set({ isDownloading: true, downloadProgress: null });
        try {
            await invoke("download_model_cmd", { modelFilename: filename });
            // Refresh model list after download
            const models = await invoke<ModelInfo[]>("list_available_models");
            set({ models, isDownloading: false, error: null });
        } catch (e) {
            set({
                isDownloading: false,
                error: e instanceof Error ? e.message : String(e),
            });
        }
    },

    // ── API endpoint ──────────────────────────────────────────────────────
    apiConfig: null,
    configureApiEndpoint: async (config: ApiEndpointConfig) => {
        try {
            await invoke("configure_api_endpoint", {
                endpoint: config.endpoint,
                apiKey: config.apiKey ?? null,
                model: config.model,
            });
            set({ apiConfig: config, error: null });
        } catch (e) {
            set({ error: e instanceof Error ? e.message : String(e) });
        }
    },
    clearApiEndpoint: () => set({ apiConfig: null }),

    // ── Settings ──────────────────────────────────────────────────────────
    settings: null,
    modelStatus: null,
    settingsOpen: false,
    settingsLoading: false,
    isDeletingModel: null,

    openSettings: () => {
        set({ settingsOpen: true });
        const { fetchSettings, fetchModels, fetchModelStatus } = get();
        fetchSettings();
        fetchModels();
        fetchModelStatus();
    },
    closeSettings: () => set({ settingsOpen: false }),

    fetchSettings: async () => {
        set({ settingsLoading: true });
        try {
            const settings = await invoke<AppSettings>("load_settings_cmd");
            set({ settings, settingsLoading: false, error: null });
        } catch (e) {
            set({
                settingsLoading: false,
                error: e instanceof Error ? e.message : String(e),
            });
        }
    },
    saveSettings: async (settings: AppSettings) => {
        try {
            await invoke("save_settings_cmd", { settings });
            set({ settings, error: null });
        } catch (e) {
            set({ error: e instanceof Error ? e.message : String(e) });
        }
    },
    fetchModelStatus: async () => {
        try {
            const modelStatus = await invoke<ModelStatus>("get_model_status");
            set({ modelStatus, error: null });
        } catch (e) {
            set({ error: e instanceof Error ? e.message : String(e) });
        }
    },
    deleteModel: async (filename: string) => {
        set({ isDeletingModel: filename });
        try {
            await invoke("delete_model_cmd", { modelFilename: filename });
            // Refresh models and model status after deletion
            const models = await invoke<ModelInfo[]>("list_available_models");
            const modelStatus = await invoke<ModelStatus>("get_model_status");
            set({ models, modelStatus, isDeletingModel: null, error: null });
        } catch (e) {
            set({
                isDeletingModel: null,
                error: e instanceof Error ? e.message : String(e),
            });
        }
    },

    // ── Credentials ───────────────────────────────────────────────────────
    saveCredential: async (key: string, value: string) => {
        await invoke("save_credential_cmd", { key, value });
    },
    loadCredential: async (key: string) => {
        const value = await invoke<string | null>("load_credential_cmd", { key });
        return value;
    },
    deleteCredential: async (key: string) => {
        await invoke("delete_credential_cmd", { key });
    },

    // ── AWS profile discovery ─────────────────────────────────────────────
    listAwsProfiles: async () => {
        try {
            return await invoke<string[]>("list_aws_profiles");
        } catch (e) {
            console.error("Failed to list AWS profiles:", e);
            return [];
        }
    },

    // ── Sessions (v2) ─────────────────────────────────────────────────────
    sessionsBrowserOpen: false,
    sessions: [],
    sessionsLoading: false,
    openSessionsBrowser: () => {
        set({ sessionsBrowserOpen: true });
        const { listSessions, purgeExpiredSessions } = get();
        // Lazy cleanup of expired trash on every open. Fire-and-forget —
        // purge failures must not block the browser from rendering.
        void purgeExpiredSessions().catch(() => {});
        // Fetch fresh on open; ignore errors (handled inside listSessions).
        // Larger limit (200) than v1's 10 — the browser has its own search/
        // sort UI, so a bigger pool makes filtering meaningful.
        void listSessions(200).catch(() => {});
    },
    closeSessionsBrowser: () => set({ sessionsBrowserOpen: false }),
    listSessions: async (limit?: number) => {
        set({ sessionsLoading: true });
        try {
            const sessions = await invoke<SessionMetadata[]>("list_sessions", {
                limit: limit ?? null,
            });
            set({ sessions, sessionsLoading: false, error: null });
            return sessions;
        } catch (e) {
            set({
                sessionsLoading: false,
                error: e instanceof Error ? e.message : String(e),
            });
            return [];
        }
    },
    loadSessionTranscript: async (sessionId: string) => {
        try {
            const segments = await invoke<TranscriptSegment[]>(
                "load_session_transcript",
                { sessionId },
            );
            // Replace current transcript view with the loaded session's segments.
            set({ transcriptSegments: segments, error: null });
            return segments;
        } catch (e) {
            set({ error: e instanceof Error ? e.message : String(e) });
            return [];
        }
    },
    // Soft-delete: flips `deleted = true` in the index; files stay on disk.
    deleteSession: async (sessionId: string) => {
        try {
            await invoke("delete_session", { sessionId });
            set((state) => ({
                sessions: state.sessions.map((s) =>
                    s.id === sessionId
                        ? { ...s, deleted: true, deleted_at: Date.now() }
                        : s,
                ),
                error: null,
            }));
        } catch (e) {
            set({ error: e instanceof Error ? e.message : String(e) });
        }
    },
    restoreSession: async (sessionId: string) => {
        try {
            await invoke("restore_session", { sessionId });
            set((state) => ({
                sessions: state.sessions.map((s) =>
                    s.id === sessionId
                        ? { ...s, deleted: false, deleted_at: null }
                        : s,
                ),
                error: null,
            }));
        } catch (e) {
            set({ error: e instanceof Error ? e.message : String(e) });
        }
    },
    deleteSessionPermanently: async (sessionId: string) => {
        try {
            await invoke("delete_session_permanently", { sessionId });
            set((state) => ({
                sessions: state.sessions.filter((s) => s.id !== sessionId),
                error: null,
            }));
        } catch (e) {
            set({ error: e instanceof Error ? e.message : String(e) });
        }
    },
    purgeExpiredSessions: async () => {
        try {
            const purged = await invoke<string[]>("purge_expired_sessions");
            if (purged.length > 0) {
                set((state) => ({
                    sessions: state.sessions.filter(
                        (s) => !purged.includes(s.id),
                    ),
                }));
            }
            return purged;
        } catch (e) {
            // Purge is best-effort housekeeping; don't stomp error state
            // because the user didn't initiate it explicitly.
            console.warn("purge_expired_sessions failed:", e);
            return [];
        }
    },
}));
