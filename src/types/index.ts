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
    model_name: string;
    bytes_downloaded: number;
    total_bytes: number | null;
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

/** Gemini status event payload (matches Rust GeminiEvent variants). */
export interface GeminiStatusEvent {
    type: "connected" | "disconnected" | "error";
    message?: string;
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
}
