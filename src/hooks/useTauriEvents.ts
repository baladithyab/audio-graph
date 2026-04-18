import { useEffect } from "react";
import { listen } from "@tauri-apps/api/event";
import i18n from "../i18n";
import { showToast } from "../components/Toast";
import { useAudioGraphStore } from "../store";
import type {
    TranscriptSegment,
    GraphSnapshot,
    PipelineStatus,
    SpeakerInfo,
    CaptureErrorPayload,
    CaptureBackpressurePayload,
    CaptureStorageFullPayload,
    GeminiTranscriptionEvent,
    GeminiResponseEvent,
    GeminiStatusEvent,
} from "../types";

// Event name constants — must match src-tauri/src/events.rs
const TRANSCRIPT_UPDATE = "transcript-update";
const GRAPH_UPDATE = "graph-update";
const PIPELINE_STATUS = "pipeline-status";
const SPEAKER_DETECTED = "speaker-detected";
const CAPTURE_ERROR = "capture-error";
const CAPTURE_BACKPRESSURE = "capture-backpressure";
const CAPTURE_STORAGE_FULL = "capture-storage-full";
const GEMINI_TRANSCRIPTION = "gemini-transcription";
const GEMINI_RESPONSE = "gemini-response";
const GEMINI_STATUS = "gemini-status";

/**
 * Hook that subscribes to all Tauri backend events and updates the Zustand store.
 * Should be called once at the app root level.
 */
export function useTauriEvents(): void {
    const addTranscriptSegment = useAudioGraphStore((s) => s.addTranscriptSegment);
    const setGraphSnapshot = useAudioGraphStore((s) => s.setGraphSnapshot);
    const setPipelineStatus = useAudioGraphStore((s) => s.setPipelineStatus);
    const addOrUpdateSpeaker = useAudioGraphStore((s) => s.addOrUpdateSpeaker);
    const setError = useAudioGraphStore((s) => s.setError);
    const setSourceBackpressure = useAudioGraphStore((s) => s.setSourceBackpressure);
    const addGeminiTranscript = useAudioGraphStore((s) => s.addGeminiTranscript);

    useEffect(() => {
        const unlisten: Array<() => void> = [];

        async function setup() {
            unlisten.push(
                await listen<TranscriptSegment>(TRANSCRIPT_UPDATE, (event) => {
                    addTranscriptSegment(event.payload);
                }),
            );

            unlisten.push(
                await listen<GraphSnapshot>(GRAPH_UPDATE, (event) => {
                    setGraphSnapshot(event.payload);
                }),
            );

            unlisten.push(
                await listen<PipelineStatus>(PIPELINE_STATUS, (event) => {
                    setPipelineStatus(event.payload);
                }),
            );

            unlisten.push(
                await listen<SpeakerInfo>(SPEAKER_DETECTED, (event) => {
                    addOrUpdateSpeaker(event.payload);
                }),
            );

            unlisten.push(
                await listen<CaptureErrorPayload>(CAPTURE_ERROR, (event) => {
                    console.error("Capture error:", event.payload);
                    setError(event.payload.error);
                }),
            );

            unlisten.push(
                await listen<CaptureBackpressurePayload>(CAPTURE_BACKPRESSURE, (event) => {
                    const { source_id, is_backpressured } = event.payload;
                    setSourceBackpressure(source_id, is_backpressured);
                }),
            );

            unlisten.push(
                await listen<CaptureStorageFullPayload>(CAPTURE_STORAGE_FULL, (event) => {
                    // Disk-full is fatal for the current capture: the writer
                    // thread has already dropped the buffer it was trying to
                    // persist. Surface a user-friendly error so the operator
                    // can free space and restart the session. Mirrors the
                    // `capture-error` subscription pattern above.
                    const { path, bytes_lost } = event.payload;
                    console.error("Storage full:", event.payload);
                    const kb = Math.max(1, Math.round(bytes_lost / 1024));
                    setError(
                        `Storage full while writing ${path}. ` +
                            `${kb} KB of transcript/graph data was lost. ` +
                            `Free disk space and restart the session.`,
                    );
                }),
            );

            // Gemini Live events
            unlisten.push(
                await listen<GeminiTranscriptionEvent>(GEMINI_TRANSCRIPTION, (event) => {
                    const { text, is_final } = event.payload;
                    addGeminiTranscript({
                        id: `gemini-${Date.now()}-${Math.random().toString(36).slice(2, 8)}`,
                        text,
                        timestamp: Date.now(),
                        is_final,
                        source: "gemini",
                    });
                }),
            );

            unlisten.push(
                await listen<GeminiResponseEvent>(GEMINI_RESPONSE, (event) => {
                    addGeminiTranscript({
                        id: `gemini-resp-${Date.now()}-${Math.random().toString(36).slice(2, 8)}`,
                        text: `[Gemini] ${event.payload.text}`,
                        timestamp: Date.now(),
                        is_final: true,
                        source: "gemini",
                    });
                }),
            );

            unlisten.push(
                await listen<GeminiStatusEvent>(GEMINI_STATUS, (event) => {
                    const { type: statusType, message, resumed } = event.payload;
                    if (statusType === "error" && message) {
                        setError(`Gemini: ${message}`);
                    } else if (statusType === "disconnected") {
                        // The backend might disconnect — update frontend state
                        useAudioGraphStore.setState({ isGeminiActive: false });
                    } else if (statusType === "reconnected") {
                        showToast({
                            variant: resumed ? "success" : "info",
                            message: i18n.t(
                                resumed
                                    ? "gemini.reconnect.resumed"
                                    : "gemini.reconnect.fresh",
                            ),
                        });
                    }
                }),
            );
        }

        setup();

        return () => {
            unlisten.forEach((fn) => fn());
        };
    }, [
        addTranscriptSegment,
        setGraphSnapshot,
        setPipelineStatus,
        addOrUpdateSpeaker,
        setError,
        setSourceBackpressure,
        addGeminiTranscript,
    ]);
}
