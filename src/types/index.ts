/**
 * IPC contract between the React frontend and the Rust backend.
 *
 * Every type in this file mirrors a serde-serialized struct or enum on
 * the Rust side (look for matching `Serialize`/`Deserialize` derives
 * under `src-tauri/src/`). Changes here require a matching change in
 * Rust — and vice versa.
 *
 * Roughly grouped into:
 *   - Audio capture (`AudioSourceInfo`, `ProcessInfo`, `AudioChunk`).
 *   - Transcript + speaker (`TranscriptSegment`, `SpeakerInfo`).
 *   - Knowledge graph (`GraphSnapshot`, `GraphNode`, `GraphLink`,
 *     `GraphDelta`, `GraphStats`).
 *   - Pipeline status + events (`PipelineStatus`, `StageStatus`,
 *     `CaptureErrorPayload`, `CaptureBackpressurePayload`,
 *     `CaptureStorageFullPayload`, `AwsErrorPayload`).
 *   - Settings (`AppSettings` and the provider sub-types).
 *   - Gemini Live events (`GeminiTranscriptionEvent`,
 *     `GeminiResponseEvent`, `GeminiStatusEvent`,
 *     `GeminiErrorCategory`, `UsageMetadata`).
 *   - Error envelope (`AppErrorPayload`) — the structured shape Rust
 *     emits when a command returns `Result<_, AppError>`. See
 *     `src-tauri/src/error.rs`.
 *   - Store type (`AudioGraphStore`) — the Zustand slice that reuses
 *     most of the IPC types above.
 *
 * `ALLOWED_CREDENTIAL_KEYS` must stay in lockstep with
 * `src-tauri/src/credentials/mod.rs::ALLOWED_CREDENTIAL_KEYS`.
 */
// Type aliases
export type SourceId = string;
export type SegmentId = string;

// Audio source types
export type AudioSourceType =
    | { type: "SystemDefault" }
    | { type: "Device"; device_id: string }
    | { type: "Application"; pid: number; app_name: string };

export interface AudioSourceInfo {
    id: SourceId;
    name: string;
    source_type: AudioSourceType;
    is_active: boolean;
}

export interface ProcessInfo {
    pid: number;
    name: string;
    exe_path: string | null;
}

// Transcript types
export interface TranscriptSegment {
    id: string; // UUID
    source_id: SourceId;
    speaker_id: string | null;
    speaker_label: string | null;
    text: string;
    start_time: number; // seconds since capture start
    end_time: number;
    confidence: number;
}

// ---------------------------------------------------------------------------
// Knowledge graph internal types
// ---------------------------------------------------------------------------

export interface GraphEntity {
    id: string;
    name: string;
    entity_type: string; // PERSON, ORG, LOCATION, EVENT, CONCEPT
    mention_count: number;
    first_seen: number;
    last_seen: number;
    aliases: string[];
    description?: string;
    speakers: string[];
}

// ---------------------------------------------------------------------------
// react-force-graph compatible types (sent from backend via events)
// ---------------------------------------------------------------------------

/** A graph node ready for react-force-graph rendering. */
export interface GraphNode {
    id: string;
    name: string;
    entity_type: string;
    /** Node size (based on mention_count). */
    val: number;
    /** Hex color by entity_type. */
    color: string;
    first_seen: number;
    last_seen: number;
    mention_count: number;
    description?: string;
}

/** A graph link ready for react-force-graph rendering. */
export interface GraphLink {
    /** Source node id. */
    source: string;
    /** Target node id. */
    target: string;
    relation_type: string;
    weight: number;
    color: string;
    label?: string;
}

/** Aggregate graph statistics. */
export interface GraphStats {
    total_nodes: number;
    total_edges: number;
    total_episodes: number;
}

/** A point-in-time snapshot of the knowledge graph for frontend rendering. */
export interface GraphSnapshot {
    /** All nodes in react-force-graph format. */
    nodes: GraphNode[];
    /** All links in react-force-graph format. */
    links: GraphLink[];
    /** Aggregate statistics. */
    stats: GraphStats;
}

// Pipeline status types
export type StageStatus =
    | { type: "Idle" }
    | { type: "Running"; processed_count: number }
    | { type: "Error"; message: string };

export interface PipelineStatus {
    capture: StageStatus;
    pipeline: StageStatus;
    asr: StageStatus;
    diarization: StageStatus;
    entity_extraction: StageStatus;
    graph: StageStatus;
}

// Speaker types
export interface SpeakerInfo {
    id: string;
    label: string;
    color: string; // hex color for UI
    total_speaking_time: number; // seconds
    segment_count: number;
}

// Capture configuration
export interface CaptureSessionConfig {
    source_id: SourceId;
    sample_rate?: number;
    channels?: number;
}

// Event payloads
export interface CaptureErrorPayload {
    source_id: string;
    error: string;
    recoverable: boolean;
}

export interface CaptureBackpressurePayload {
    source_id: string;
    is_backpressured: boolean;
}

/**
 * Payload for `capture-storage-full` events — emitted when a persistence
 * write (transcript JSONL, graph snapshot) fails because the underlying
 * storage is full. `bytes_written` is best-effort and may be `0` when the
 * error happened on the initial open; `bytes_lost` is the size of the
 * buffer the app was trying to persist.
 */
export interface CaptureStorageFullPayload {
    path: string;
    bytes_written: number;
    bytes_lost: number;
}

// ---------------------------------------------------------------------------
// AWS error taxonomy (ag#13)
// ---------------------------------------------------------------------------

/**
 * Structured classification of aws-sdk errors surfaced by the backend, keyed
 * on `category`. Matches Rust `crate::aws_util::UiAwsError` serialized with
 * `#[serde(tag = "category", rename_all = "snake_case")]`.
 *
 * The frontend uses `category` to pick an `aws.error.*` i18n key and to
 * decide which recovery hint to show (e.g. "check Settings → AWS").
 */
export type UiAwsError =
    | { category: "invalid_access_key" }
    | { category: "signature_mismatch" }
    | { category: "expired_token" }
    | { category: "access_denied"; permission: string | null }
    | { category: "region_not_supported"; region: string }
    | { category: "network_unreachable" }
    | { category: "unknown"; message: string };

/**
 * Payload for the `aws-error` event (ag#13). `error` is the structured
 * classification; `raw_message` is the original aws-sdk error string,
 * retained for debugging / disclosure when the category is `unknown`.
 */
export interface AwsErrorPayload {
    error: UiAwsError;
    raw_message: string;
}

// ---------------------------------------------------------------------------
// Model management types
// ---------------------------------------------------------------------------

export interface ModelInfo {
    name: string;
    filename: string;
    url: string;
    size_bytes: number | null;
    is_downloaded: boolean;
    is_valid: boolean;
    description: string;
    local_path: string | null;
}

export interface DownloadProgress {
    /** Stable identifier — matches `ModelInfo.filename`. */
    model_id: string;
    /** Display name kept for legacy consumers keyed off the friendly label. */
    model_name: string;
    bytes_downloaded: number;
    /** `0` when the server omitted `Content-Length` (treat as unknown). */
    total_bytes: number;
    /** Wall-clock milliseconds since the download started. */
    elapsed_ms: number;
    percent: number;
    /** One of: "downloading", "complete", "error" */
    status: string;
}

// ---------------------------------------------------------------------------
// API endpoint configuration
// ---------------------------------------------------------------------------

/** Configuration for an OpenAI-compatible API endpoint. */
export interface ApiEndpointConfig {
    /** Base URL, e.g. "https://openrouter.ai/api/v1" or "http://localhost:11434/v1" */
    endpoint: string;
    /** Bearer token. Omit for local servers (Ollama, LM Studio). */
    apiKey?: string;
    /** Model identifier, e.g. "gpt-4o-mini", "llama3.2", "qwen2.5:3b" */
    model: string;
}

// ---------------------------------------------------------------------------
// Settings & model readiness types
// ---------------------------------------------------------------------------

/** Model readiness status (matches Rust ModelReadiness enum) */
export type ModelReadiness = "Ready" | "NotDownloaded" | "Invalid";

/** Aggregate model status (matches Rust ModelStatus struct) */
export interface ModelStatus {
    whisper: ModelReadiness;
    llm: ModelReadiness;
    sortformer: ModelReadiness;
}

/** AWS credential source (matches Rust AwsCredentialSource enum with serde tag) */
export type AwsCredentialSource =
    | { type: "default_chain" }
    | { type: "profile"; name: string }
    | { type: "access_keys"; access_key: string };

/** ASR provider configuration (matches Rust AsrProvider enum with serde tag) */
export type AsrProvider =
    | { type: "local_whisper" }
    | { type: "api"; endpoint: string; api_key: string; model: string }
    | { type: "aws_transcribe"; region: string; language_code: string; credential_source: AwsCredentialSource; enable_diarization: boolean }
    | { type: "deepgram"; api_key: string; model: string; enable_diarization: boolean }
    | { type: "assemblyai"; api_key: string; enable_diarization: boolean }
    | { type: "sherpa_onnx"; model_dir: string; enable_endpoint_detection: boolean };

/** LLM provider configuration (matches Rust LlmProvider enum with serde tag) */
export type LlmProvider =
    | { type: "local_llama" }
    | { type: "api"; endpoint: string; api_key: string; model: string }
    | { type: "aws_bedrock"; region: string; model_id: string; credential_source: AwsCredentialSource }
    | { type: "mistralrs"; model_id: string };

/** LLM API configuration for persistence */
export interface LlmApiConfig {
    endpoint: string;
    api_key: string | null;
    model: string;
    max_tokens: number;
    temperature: number;
}

/** Audio processing settings */
export interface AudioSettings {
    sample_rate: number;
    channels: number;
}

/** Top-level application settings (matches Rust AppSettings) */
export interface AppSettings {
    asr_provider: AsrProvider;
    whisper_model: string;
    llm_provider: LlmProvider;
    llm_api_config: LlmApiConfig | null;
    audio_settings: AudioSettings;
    gemini: GeminiSettings;
    /**
     * Runtime log-verbosity preference. One of
     * "off" | "error" | "warn" | "info" | "debug" | "trace".
     * Optional because older settings files won't have it; backend
     * treats `undefined` / missing as "info".
     */
    log_level?: string;
    /**
     * Demo mode — set once on first launch when no cloud credentials are
     * present. `undefined` means "not yet decided"; `true` means the app is
     * running local-only and the demo banner should show until models are
     * downloaded; `false` means the user has already configured providers.
     */
    demo_mode?: boolean;
}

// ---------------------------------------------------------------------------
// Gemini types
// ---------------------------------------------------------------------------

/** Gemini transcription event payload (matches Rust GeminiEvent::Transcription). */
export interface GeminiTranscriptionEvent {
    type: "transcription";
    text: string;
    is_final: boolean;
}

/** Gemini model response event payload (matches Rust GeminiEvent::ModelResponse). */
export interface GeminiResponseEvent {
    type: "model_response";
    text: string;
}

/** Per-modality token count (matches Rust ModalityTokenCount). */
export interface ModalityTokenCount {
    modality: string;
    tokenCount: number;
}

/**
 * Token usage metadata from Gemini Live `usageMetadata` frames.
 * Matches Rust {@link UsageMetadata} (camelCase preserved via serde).
 *
 * All counters are optional: the server only populates fields that are
 * meaningful for the current frame, and `undefined` means "not reported"
 * (distinct from `0`, which means "reported as zero"). Detail arrays are
 * empty when the server omits them.
 */
export interface UsageMetadata {
    promptTokenCount?: number;
    cachedContentTokenCount?: number;
    responseTokenCount?: number;
    toolUsePromptTokenCount?: number;
    thoughtsTokenCount?: number;
    totalTokenCount?: number;
    promptTokensDetails?: ModalityTokenCount[];
    cacheTokensDetails?: ModalityTokenCount[];
    responseTokensDetails?: ModalityTokenCount[];
    toolUsePromptTokensDetails?: ModalityTokenCount[];
}

/**
 * Categorized failure reason attached to every `gemini-status` event of
 * type `"error"`. Matches Rust {@link GeminiErrorCategory} (snake_case via
 * serde). The `kind` field is the routing key for i18n + toast severity
 * (auth/authExpired/rateLimit → warning, network → info, server/unknown
 * → error). See `gemini/mod.rs::classify_close_frame` /
 * `classify_tungstenite_error` for the mapping rules.
 */
export type GeminiErrorCategory =
    | { kind: "auth" }
    | { kind: "auth_expired" }
    | { kind: "rate_limit"; retry_after_secs?: number }
    | { kind: "server" }
    | { kind: "network" }
    | { kind: "unknown" };

/** Gemini status event payload (matches Rust GeminiEvent variants). */
export interface GeminiStatusEvent {
    type:
        | "connected"
        | "disconnected"
        | "error"
        | "reconnecting"
        | "reconnected"
        | "turn_complete";
    message?: string;
    /**
     * Present on `error` events. Carries the structured classification
     * determined at the error site so the frontend can route to the
     * correct i18n key + toast severity without re-parsing `message`.
     */
    category?: GeminiErrorCategory;
    /** Present on `reconnecting` events — 1-based retry number. */
    attempt?: number;
    /** Present on `reconnecting` events — seconds until the next retry. */
    backoff_secs?: number;
    /**
     * Present on `reconnected` events. `true` means the reconnect used a
     * cached session-resumption handle (prior conversation context was
     * requested from the server); `false` means the new socket started from
     * a fresh session. Hint only — server-side rejection of the handle is
     * not observable here.
     */
    resumed?: boolean;
    /**
     * Present on `turn_complete` events when the server attached a
     * `usageMetadata` block to this frame. `undefined` when the frame
     * carries no usage accounting (e.g. mid-stream turn boundaries). The
     * frontend can safely sum `totalTokenCount` across turns for
     * cumulative session usage.
     */
    usage?: UsageMetadata;
}

/** A single Gemini transcript entry for display. */
export interface GeminiTranscriptEntry {
    id: string;
    text: string;
    timestamp: number;
    is_final: boolean;
    source: "gemini";
}

/** Gemini auth mode (matches Rust GeminiAuthMode enum with serde tag). */
export type GeminiAuthMode =
    | { type: "api_key"; api_key: string }
    | { type: "vertex_ai"; project_id: string; location: string; service_account_path?: string };

/** Gemini settings (matches Rust GeminiSettings). */
export interface GeminiSettings {
    auth: GeminiAuthMode;
    model: string;
}

// ---------------------------------------------------------------------------
// Session management types (v1: list + load transcript + delete)
// ---------------------------------------------------------------------------

export interface SessionMetadata {
    id: string;
    title: string | null;
    created_at: number;      // unix millis
    ended_at: number | null; // unix millis
    duration_seconds: number | null;
    status: "active" | "complete" | "crashed";
    segment_count: number;
    speaker_count: number;
    entity_count: number;
    transcript_path: string;
    graph_path: string;
    /**
     * Soft-delete flag. Trashed sessions stay on disk but are hidden from
     * the default list view. Older sessions.json files (pre-SessionsBrowser
     * v2) omit this field — treat `undefined` as `false`.
     */
    deleted?: boolean;
    /**
     * Unix-millis timestamp of when the session was soft-deleted. Used for
     * the 30-day retention countdown before auto-purge.
     */
    deleted_at?: number | null;
}

/**
 * Per-session token usage record returned by `get_session_usage` /
 * `get_current_session_usage`. Matches Rust `sessions::usage::SessionUsage`
 * (snake_case preserved by serde).
 */
export interface SessionUsage {
    session_id: string;
    prompt: number;
    response: number;
    cached: number;
    thoughts: number;
    tool_use: number;
    total: number;
    turns: number;
    /** Unix millis of the last update; `0` means never updated. */
    updated_at: number;
}

/**
 * Aggregate token usage across every `~/.audiograph/usage/*.json` file.
 * Returned by the `get_lifetime_usage` command. Has no `session_id` — it's
 * a sum. `sessions` counts how many session files contributed.
 */
export interface LifetimeUsage {
    prompt: number;
    response: number;
    cached: number;
    thoughts: number;
    tool_use: number;
    total: number;
    turns: number;
    sessions: number;
}

// ---------------------------------------------------------------------------
// Structured error payloads (matches Rust AppError enum)
// ---------------------------------------------------------------------------

/**
 * Structured error payload emitted by commands that return `Result<T, AppError>`.
 *
 * Shape: `{ code: "<snake_case>", message: <variant-specific-payload> }`.
 * Unit variants (e.g. `aws_credential_expired`) omit the `message` key
 * entirely — serde's internally-tagged enum does not emit `null` for empty
 * content. The `message` field is therefore `null | undefined` for those.
 *
 * This is a **pilot** (loop10 MEDIUM #8): only `save_credential_cmd` and
 * `start_transcribe` reject with this shape today. Most commands still
 * reject with plain strings — `errorToMessage` falls back to `String(e)`
 * for that case.
 */
export type AppErrorPayload =
    | { code: "io"; message: string }
    | { code: "credential_missing"; message: { key: string } }
    | { code: "credential_file_error"; message: { reason: string } }
    | { code: "aws_credential_expired"; message?: null }
    | { code: "aws_region_invalid"; message: { region: string } }
    | { code: "gemini_rate_limited"; message?: null }
    | { code: "model_not_found"; message: { name: string } }
    | { code: "session_invalid"; message: { reason: string } }
    | { code: "network_timeout"; message: { service: string } }
    | { code: "unknown"; message: string };

/**
 * Canonical list of credential keys accepted by the `save_credential`,
 * `load_credential`, and `delete_credential` Tauri commands.
 *
 * IMPORTANT: this list must stay in sync with the Rust constant
 * `ALLOWED_CREDENTIAL_KEYS` in `src-tauri/src/credentials/mod.rs`. There
 * is no runtime cross-check — this is a convention only. If you add or
 * remove a credential field, update both places.
 */
export const ALLOWED_CREDENTIAL_KEYS: readonly string[] = [
    "openai_api_key",
    "groq_api_key",
    "together_api_key",
    "fireworks_api_key",
    "deepgram_api_key",
    "assemblyai_api_key",
    "gemini_api_key",
    "google_service_account_path",
    "aws_access_key",
    "aws_secret_key",
    "aws_session_token",
    "aws_profile",
    "aws_region",
];

/** Credential store for sensitive API keys. */
export interface CredentialStore {
    openai_api_key?: string;
    groq_api_key?: string;
    together_api_key?: string;
    fireworks_api_key?: string;
    deepgram_api_key?: string;
    assemblyai_api_key?: string;
    gemini_api_key?: string;
    google_service_account_path?: string;
    aws_access_key?: string;
    aws_secret_key?: string;
    aws_session_token?: string;
    aws_profile?: string;
    aws_region?: string;
}

// ---------------------------------------------------------------------------
// Chat types
// ---------------------------------------------------------------------------

export interface ChatMessage {
    role: "user" | "assistant" | "system";
    content: string;
}

export interface ChatResponse {
    message: ChatMessage;
    tokens_used: number;
}

// ---------------------------------------------------------------------------
// Store type
// ---------------------------------------------------------------------------

/** Shape of the Zustand audio-graph store. */
export interface AudioGraphStore {
    // Audio sources
    audioSources: AudioSourceInfo[];
    selectedSourceIds: string[];
    setAudioSources: (sources: AudioSourceInfo[]) => void;
    toggleSourceId: (id: string) => void;
    clearSelectedSources: () => void;
    fetchSources: () => Promise<void>;

    // Processes
    processes: ProcessInfo[];
    searchFilter: string;
    fetchProcesses: () => Promise<void>;
    setSearchFilter: (filter: string) => void;

    // Transcript
    transcriptSegments: TranscriptSegment[];
    addTranscriptSegment: (segment: TranscriptSegment) => void;
    clearTranscript: () => void;

    // Knowledge graph
    graphSnapshot: GraphSnapshot;
    setGraphSnapshot: (snapshot: GraphSnapshot) => void;

    // Exports (backend → JSON string)
    exportTranscript: () => Promise<string>;
    exportGraph: () => Promise<string>;
    getSessionId: () => Promise<string>;

    // Pipeline status
    pipelineStatus: PipelineStatus;
    setPipelineStatus: (status: PipelineStatus) => void;

    // Speakers
    speakers: SpeakerInfo[];
    addOrUpdateSpeaker: (speaker: SpeakerInfo) => void;
    clearSpeakers: () => void;

    // Capture state
    isCapturing: boolean;
    captureStartTime: number | null;
    setIsCapturing: (capturing: boolean) => void;
    startCapture: () => Promise<void>;
    stopCapture: () => Promise<void>;

    /// IDs of sources currently reporting backpressure. Updated by the
    /// `capture-backpressure` event listener. Non-empty means at least one
    /// active source's ring buffer is dropping chunks — surface a warning in
    /// the UI so the user can slow the pipeline (e.g. disable Gemini) before
    /// transcript quality degrades.
    backpressuredSources: string[];
    setSourceBackpressure: (sourceId: string, isBackpressured: boolean) => void;

    // Transcribe state (manual transcription)
    isTranscribing: boolean;
    startTranscribe: () => Promise<void>;
    stopTranscribe: () => Promise<void>;

    // Error state
    error: string | null;
    setError: (error: string | null) => void;
    clearError: () => void;

    // ── Chat ─────────────────────────────────────────────────────────────
    chatMessages: ChatMessage[];
    isChatLoading: boolean;
    rightPanelTab: "transcript" | "chat";
    setRightPanelTab: (tab: "transcript" | "chat") => void;
    sendChatMessage: (message: string) => Promise<void>;
    clearChatHistory: () => Promise<void>;

    // ── Models ────────────────────────────────────────────────────────────
    models: ModelInfo[];
    isDownloading: boolean;
    downloadProgress: DownloadProgress | null;
    fetchModels: () => Promise<void>;
    downloadModel: (filename: string) => Promise<void>;

    // ── API endpoint ──────────────────────────────────────────────────────
    apiConfig: ApiEndpointConfig | null;
    configureApiEndpoint: (config: ApiEndpointConfig) => Promise<void>;
    clearApiEndpoint: () => void;

    // ── Gemini Live dual pipeline ───────────────────────────────────────────
    isGeminiActive: boolean;
    geminiTranscripts: GeminiTranscriptEntry[];
    addGeminiTranscript: (entry: GeminiTranscriptEntry) => void;
    clearGeminiTranscripts: () => void;
    startGemini: () => Promise<void>;
    stopGemini: () => Promise<void>;

    // ── Settings ──────────────────────────────────────────────────────────
    settings: AppSettings | null;
    modelStatus: ModelStatus | null;
    settingsOpen: boolean;
    settingsLoading: boolean;
    isDeletingModel: string | null;
    openSettings: () => void;
    closeSettings: () => void;
    fetchSettings: () => Promise<void>;
    saveSettings: (settings: AppSettings) => Promise<void>;
    fetchModelStatus: () => Promise<void>;
    deleteModel: (filename: string) => Promise<void>;

    // ── Credentials ──────────────────────────────────────────────────────
    saveCredential: (key: string, value: string) => Promise<void>;
    loadCredential: (key: string) => Promise<string | null>;
    deleteCredential: (key: string) => Promise<void>;

    // ── AWS profile discovery ────────────────────────────────────────────
    /** List profile names discovered in ~/.aws/config and ~/.aws/credentials. */
    listAwsProfiles: () => Promise<string[]>;

    // ── Sessions (v2: list, load transcript, soft-delete + restore) ──────
    sessionsBrowserOpen: boolean;
    sessions: SessionMetadata[];
    sessionsLoading: boolean;
    openSessionsBrowser: () => void;
    closeSessionsBrowser: () => void;
    listSessions: (limit?: number) => Promise<SessionMetadata[]>;
    loadSessionTranscript: (sessionId: string) => Promise<TranscriptSegment[]>;
    /** Soft-delete: flag as trashed, files stay on disk, restorable. */
    deleteSession: (sessionId: string) => Promise<void>;
    /** Restore a soft-deleted session back to the active list. */
    restoreSession: (sessionId: string) => Promise<void>;
    /** Permanently delete a session (unlinks files). Bypasses trash. */
    deleteSessionPermanently: (sessionId: string) => Promise<void>;
    /** Lazy cleanup: ask backend to hard-delete trash entries older than 30d. */
    purgeExpiredSessions: () => Promise<string[]>;
}
