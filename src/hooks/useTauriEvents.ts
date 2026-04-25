/**
 * Tauri backend-event bridge.
 *
 * Call `useTauriEvents()` once at the root (see `App.tsx`). The hook
 * subscribes to every backend event the app cares about and funnels
 * each payload into the Zustand store or into a side-effect publisher:
 *
 *   - `TRANSCRIPT_UPDATE`       → `addTranscriptSegment`
 *   - `GRAPH_UPDATE`            → `setGraphSnapshot`
 *   - `PIPELINE_STATUS`         → `setPipelineStatus`
 *   - `SPEAKER_DETECTED`        → `addOrUpdateSpeaker`
 *   - `CAPTURE_ERROR`           → `setError`
 *   - `CAPTURE_BACKPRESSURE`    → `setSourceBackpressure`
 *   - `CAPTURE_STORAGE_FULL`    → `publishStorageFull` (StorageBanner)
 *   - `GEMINI_TRANSCRIPTION`    → `addGeminiTranscript`
 *   - `GEMINI_RESPONSE`         → `addGeminiTranscript`
 *   - `MODEL_DOWNLOAD_PROGRESS` → `downloadProgress` store slice
 *   - `GEMINI_STATUS`           → classified toast + store update
 *   - `AWS_ERROR`               → `setError` (localized via
 *                                 `awsErrorToMessage`)
 *
 * The event names are duplicated here as top-of-file string constants
 * so tests can assert on them; they must stay in sync with the Rust
 * constants in `src-tauri/src/events.rs`.
 *
 * Error-routing helpers `routeGeminiError` and `awsErrorToMessage` are
 * exported so unit tests and potential future diagnostics surfaces can
 * reuse the exact same classification without duplicating the switch
 * statements.
 */
import { useEffect } from "react";
import { listen } from "@tauri-apps/api/event";
import i18n from "../i18n";
import { showToast } from "../components/Toast";
import { publishStorageFull } from "../components/StorageBanner";
import { useAudioGraphStore } from "../store";
import type {
    TranscriptSegment,
    GraphSnapshot,
    PipelineStatus,
    SpeakerInfo,
    CaptureErrorPayload,
    CaptureBackpressurePayload,
    CaptureStorageFullPayload,
    DownloadProgress,
    GeminiTranscriptionEvent,
    GeminiResponseEvent,
    GeminiStatusEvent,
    GeminiErrorCategory,
    AwsErrorPayload,
} from "../types";
import type { ToastVariant } from "../components/Toast";

/**
 * Map a classified Gemini error category to its i18n key + toast variant.
 *
 * Routing rules (ag#10 spec):
 *   auth, auth_expired, rate_limit → warning (user action required,
 *                                             not a crash)
 *   network                        → info     (likely transient; the
 *                                              reconnect loop will retry)
 *   server, unknown                → error    (genuinely broken)
 *
 * Keys live under `gemini.error.*` so translators can group them.
 */
export function routeGeminiError(
    category: GeminiErrorCategory,
): { key: string; variant: ToastVariant } {
    switch (category.kind) {
        case "auth":
            return { key: "gemini.error.auth", variant: "warning" };
        case "auth_expired":
            return { key: "gemini.error.authExpired", variant: "warning" };
        case "rate_limit":
            return { key: "gemini.error.rateLimit", variant: "warning" };
        case "network":
            return { key: "gemini.error.network", variant: "info" };
        case "server":
            return { key: "gemini.error.server", variant: "error" };
        case "unknown":
        default:
            return { key: "gemini.error.unknown", variant: "error" };
    }
}

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
const MODEL_DOWNLOAD_PROGRESS = "model-download-progress";
const AWS_ERROR = "aws-error";

/**
 * Translate a structured {@link AwsErrorPayload} (ag#13) into a user-facing
 * message via the `aws.error.*` i18n namespace. Exported so unit tests and
 * any future in-app diagnostics panel can share the exact same mapping
 * without duplicating the switch.
 */
export function awsErrorToMessage(payload: AwsErrorPayload): string {
    const { error } = payload;
    switch (error.category) {
        case "invalid_access_key":
            return i18n.t("aws.error.invalidAccessKey");
        case "signature_mismatch":
            return i18n.t("aws.error.signatureMismatch");
        case "expired_token":
            return i18n.t("aws.error.expiredToken");
        case "access_denied":
            return i18n.t("aws.error.accessDenied", {
                // `permission` is `null` when the backend could not parse
                // the action out of the AWS message — the i18n copy falls
                // back to a generic "check your IAM policy" hint.
                permission: error.permission ?? "",
            });
        case "region_not_supported":
            return i18n.t("aws.error.regionNotSupported", {
                region: error.region,
            });
        case "network_unreachable":
            return i18n.t("aws.error.networkUnreachable");
        case "unknown":
            return i18n.t("aws.error.unknown", { message: error.message });
    }
}

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
        let unlisten: Array<(() => void) | null> = [];

        async function safeListen<T>(
            eventName: string,
            cb: (event: { payload: T }) => void,
        ): Promise<(() => void) | null> {
            try {
                return await listen<T>(eventName, cb as never);
            } catch (err) {
                console.error(`Failed to subscribe to ${eventName}:`, err);
                return null;
            }
        }

        async function setup() {
            unlisten = await Promise.all([
                safeListen<TranscriptSegment>(TRANSCRIPT_UPDATE, (event) => {
                    addTranscriptSegment(event.payload);
                }),
                safeListen<GraphSnapshot>(GRAPH_UPDATE, (event) => {
                    setGraphSnapshot(event.payload);
                }),
                safeListen<PipelineStatus>(PIPELINE_STATUS, (event) => {
                    setPipelineStatus(event.payload);
                }),
                safeListen<SpeakerInfo>(SPEAKER_DETECTED, (event) => {
                    addOrUpdateSpeaker(event.payload);
                }),
                safeListen<CaptureErrorPayload>(CAPTURE_ERROR, (event) => {
                    console.error("Capture error:", event.payload);
                    setError(event.payload.error);
                }),
                safeListen<CaptureBackpressurePayload>(CAPTURE_BACKPRESSURE, (event) => {
                    const { source_id, is_backpressured } = event.payload;
                    setSourceBackpressure(source_id, is_backpressured);
                }),
                safeListen<CaptureStorageFullPayload>(CAPTURE_STORAGE_FULL, (event) => {
                    console.error("Storage full:", event.payload);
                    publishStorageFull(event.payload);
                }),
                safeListen<GeminiTranscriptionEvent>(GEMINI_TRANSCRIPTION, (event) => {
                    const { text, is_final } = event.payload;
                    addGeminiTranscript({
                        id: `gemini-${Date.now()}-${Math.random().toString(36).slice(2, 8)}`,
                        text,
                        timestamp: Date.now(),
                        is_final,
                        source: "gemini",
                    });
                }),
                safeListen<GeminiResponseEvent>(GEMINI_RESPONSE, (event) => {
                    addGeminiTranscript({
                        id: `gemini-resp-${Date.now()}-${Math.random().toString(36).slice(2, 8)}`,
                        text: `[Gemini] ${event.payload.text}`,
                        timestamp: Date.now(),
                        is_final: true,
                        source: "gemini",
                    });
                }),
                safeListen<DownloadProgress>(MODEL_DOWNLOAD_PROGRESS, (event) => {
                    useAudioGraphStore.setState({
                        downloadProgress: event.payload,
                    });
                }),
                safeListen<GeminiStatusEvent>(GEMINI_STATUS, (event) => {
                    const {
                        type: statusType,
                        message,
                        resumed,
                        category,
                    } = event.payload;
                    if (statusType === "error") {
                        // Structured routing: prefer the classified
                        // `category` (ag#10) to pick the i18n key + toast
                        // severity. Fall back to the raw `message` in the
                        // error banner for unclassified or legacy events.
                        if (category) {
                            const { key, variant } = routeGeminiError(category);
                            const extra =
                                category.kind === "rate_limit" &&
                                typeof category.retry_after_secs === "number"
                                    ? { retry: category.retry_after_secs }
                                    : undefined;
                            showToast({
                                variant,
                                message: i18n.t(key, extra),
                            });
                        } else if (message) {
                            setError(`Gemini: ${message}`);
                        }
                    } else if (statusType === "disconnected") {
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
                safeListen<AwsErrorPayload>(AWS_ERROR, (event) => {
                    console.error("AWS error:", event.payload);
                    // Route structured AWS errors through the error banner
                    // (same UI path as other blocking errors) with a
                    // localized, actionable message built from the
                    // category-specific i18n key.
                    setError(awsErrorToMessage(event.payload));
                }),
            ]);
        }

        setup();

        return () => {
            for (const fn of unlisten) {
                if (fn) fn();
            }
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
