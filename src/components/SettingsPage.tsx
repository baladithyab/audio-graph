import { useEffect, useState } from "react";
import { useAudioGraphStore } from "../store";
import type {
  AsrProvider,
  AwsCredentialSource,
  GeminiAuthMode,
  GeminiSettings,
  LlmApiConfig,
  LlmProvider,
  ModelReadiness,
} from "../types";

/** Format bytes to a human-readable size string (e.g. "466 MB"). */
function formatSize(bytes: number | null): string {
  if (bytes === null || bytes === 0) return "—";
  const mb = bytes / (1024 * 1024);
  if (mb >= 1024) {
    return `${(mb / 1024).toFixed(1)} GB`;
  }
  return `${Math.round(mb)} MB`;
}

/** Map a ModelReadiness value to a CSS modifier and label. */
function readinessBadge(status: ModelReadiness): {
  cls: string;
  label: string;
} {
  switch (status) {
    case "Ready":
      return { cls: "status-badge--ready", label: "Ready" };
    case "NotDownloaded":
      return { cls: "status-badge--not-downloaded", label: "Not Downloaded" };
    case "Invalid":
      return { cls: "status-badge--invalid", label: "Invalid" };
  }
}

function SettingsPage() {
  const {
    settings,
    models,
    modelStatus,
    settingsLoading,
    isDownloading,
    downloadProgress,
    isDeletingModel,
    closeSettings,
    saveSettings,
    downloadModel,
    deleteModel,
  } = useAudioGraphStore();

  // ── Local form state ──────────────────────────────────────────────────
  const [asrType, setAsrType] = useState<
    "local_whisper" | "api" | "aws_transcribe" | "deepgram" | "assemblyai" | "sherpa_onnx"
  >("local_whisper");
  const [whisperModel, setWhisperModel] = useState("ggml-small.en.bin");
  const [asrEndpoint, setAsrEndpoint] = useState("");
  const [asrApiKey, setAsrApiKey] = useState("");
  const [asrModel, setAsrModel] = useState("");

  // AWS Transcribe fields
  const [awsAsrRegion, setAwsAsrRegion] = useState("us-east-1");
  const [awsAsrLanguageCode, setAwsAsrLanguageCode] = useState("en-US");
  const [awsAsrCredentialMode, setAwsAsrCredentialMode] = useState<
    "default_chain" | "profile" | "access_keys"
  >("default_chain");
  const [awsAsrProfileName, setAwsAsrProfileName] = useState("");
  const [awsAsrAccessKey, setAwsAsrAccessKey] = useState("");
  const [awsAsrDiarization, setAwsAsrDiarization] = useState(true);

  // Deepgram fields
  const [deepgramApiKey, setDeepgramApiKey] = useState("");
  const [deepgramModel, setDeepgramModel] = useState("nova-3");
  const [deepgramDiarization, setDeepgramDiarization] = useState(true);

  // AssemblyAI fields
  const [assemblyaiApiKey, setAssemblyaiApiKey] = useState("");
  const [assemblyaiDiarization, setAssemblyaiDiarization] = useState(true);

  // Sherpa-ONNX fields
  const [sherpaModelDir, setSherpaModelDir] = useState("streaming-zipformer-en-20M");
  const [sherpaEndpointDetection, setSherpaEndpointDetection] = useState(true);

  const [llmType, setLlmType] = useState<"local_llama" | "api" | "aws_bedrock" | "mistralrs">(
    "api",
  );
  const [llmEndpoint, setLlmEndpoint] = useState("http://localhost:11434/v1");
  const [llmApiKey, setLlmApiKey] = useState("");
  const [llmModel, setLlmModel] = useState("llama3.2");
  const [llmMaxTokens, setLlmMaxTokens] = useState(2048);
  const [llmTemperature, setLlmTemperature] = useState(0.7);

  // Mistral.rs fields
  const [mistralrsModelId, setMistralrsModelId] = useState("ggml-small-extract.gguf");

  // AWS Bedrock fields
  const [awsBedrockRegion, setAwsBedrockRegion] = useState("us-east-1");
  const [awsBedrockModelId, setAwsBedrockModelId] = useState("");
  const [awsBedrockCredentialMode, setAwsBedrockCredentialMode] = useState<
    "default_chain" | "profile" | "access_keys"
  >("default_chain");
  const [awsBedrockProfileName, setAwsBedrockProfileName] = useState("");
  const [awsBedrockAccessKey, setAwsBedrockAccessKey] = useState("");

  // Gemini settings
  const [geminiAuthMode, setGeminiAuthMode] = useState<"api_key" | "vertex_ai">(
    "api_key",
  );
  const [geminiApiKey, setGeminiApiKey] = useState("");
  const [geminiModel, setGeminiModel] = useState("gemini-3.1-flash-live-preview");
  const [geminiProjectId, setGeminiProjectId] = useState("");
  const [geminiLocation, setGeminiLocation] = useState("");
  const [geminiServiceAccountPath, setGeminiServiceAccountPath] = useState("");

  const [confirmDelete, setConfirmDelete] = useState<string | null>(null);

  // Sync local state when settings are loaded
  useEffect(() => {
    if (!settings) return;

    // Whisper model selection
    if (settings.whisper_model) {
      setWhisperModel(settings.whisper_model);
    }

    // ASR provider
    const asr = settings.asr_provider;
    setAsrType(asr.type);
    if (asr.type === "api") {
      setAsrEndpoint(asr.endpoint);
      setAsrApiKey(asr.api_key);
      setAsrModel(asr.model);
    } else if (asr.type === "aws_transcribe") {
      setAwsAsrRegion(asr.region);
      setAwsAsrLanguageCode(asr.language_code);
      setAwsAsrDiarization(asr.enable_diarization);
      const cred = asr.credential_source;
      setAwsAsrCredentialMode(cred.type);
      if (cred.type === "profile") setAwsAsrProfileName(cred.name);
      if (cred.type === "access_keys") setAwsAsrAccessKey(cred.access_key);
    } else if (asr.type === "deepgram") {
      setDeepgramApiKey(asr.api_key);
      setDeepgramModel(asr.model);
      setDeepgramDiarization(asr.enable_diarization);
    } else if (asr.type === "assemblyai") {
      setAssemblyaiApiKey(asr.api_key);
      setAssemblyaiDiarization(asr.enable_diarization);
    } else if (asr.type === "sherpa_onnx") {
      setAsrType("sherpa_onnx");
      setSherpaModelDir(asr.model_dir);
      setSherpaEndpointDetection(asr.enable_endpoint_detection);
    }

    // LLM provider
    const llm = settings.llm_provider;
    setLlmType(llm.type);
    if (llm.type === "api") {
      setLlmEndpoint(llm.endpoint);
      setLlmApiKey(llm.api_key);
      setLlmModel(llm.model);
    } else if (llm.type === "aws_bedrock") {
      setAwsBedrockRegion(llm.region);
      setAwsBedrockModelId(llm.model_id);
      const cred = llm.credential_source;
      setAwsBedrockCredentialMode(cred.type);
      if (cred.type === "profile") setAwsBedrockProfileName(cred.name);
      if (cred.type === "access_keys") setAwsBedrockAccessKey(cred.access_key);
    } else if (settings.llm_provider.type === "mistralrs") {
      setLlmType("mistralrs");
      setMistralrsModelId(settings.llm_provider.model_id);
    }

    // LLM config (advanced — max_tokens / temperature)
    if (settings.llm_api_config) {
      setLlmMaxTokens(settings.llm_api_config.max_tokens);
      setLlmTemperature(settings.llm_api_config.temperature);
    }

    // Gemini settings
    if (settings.gemini) {
      setGeminiModel(settings.gemini.model);
      const auth = settings.gemini.auth;
      setGeminiAuthMode(auth.type);
      if (auth.type === "api_key") {
        setGeminiApiKey(auth.api_key);
      } else if (auth.type === "vertex_ai") {
        setGeminiProjectId(auth.project_id);
        setGeminiLocation(auth.location);
        setGeminiServiceAccountPath(auth.service_account_path ?? "");
      }
    }
  }, [settings]);

  // ── Helpers ───────────────────────────────────────────────────────────
  const buildAwsCredentialSource = (
    mode: "default_chain" | "profile" | "access_keys",
    profileName: string,
    accessKey: string,
  ): AwsCredentialSource => {
    switch (mode) {
      case "profile":
        return { type: "profile", name: profileName };
      case "access_keys":
        return { type: "access_keys", access_key: accessKey };
      default:
        return { type: "default_chain" };
    }
  };

  // ── Handlers ──────────────────────────────────────────────────────────
  const handleSave = async () => {
    let asrProvider: AsrProvider;
    switch (asrType) {
      case "api":
        asrProvider = {
          type: "api",
          endpoint: asrEndpoint,
          api_key: asrApiKey,
          model: asrModel,
        };
        break;
      case "aws_transcribe":
        asrProvider = {
          type: "aws_transcribe",
          region: awsAsrRegion,
          language_code: awsAsrLanguageCode,
          credential_source: buildAwsCredentialSource(
            awsAsrCredentialMode,
            awsAsrProfileName,
            awsAsrAccessKey,
          ),
          enable_diarization: awsAsrDiarization,
        };
        break;
      case "deepgram":
        asrProvider = {
          type: "deepgram",
          api_key: deepgramApiKey,
          model: deepgramModel,
          enable_diarization: deepgramDiarization,
        };
        break;
      case "assemblyai":
        asrProvider = {
          type: "assemblyai",
          api_key: assemblyaiApiKey,
          enable_diarization: assemblyaiDiarization,
        };
        break;
      case "sherpa_onnx":
        asrProvider = {
          type: "sherpa_onnx",
          model_dir: sherpaModelDir,
          enable_endpoint_detection: sherpaEndpointDetection,
        };
        break;
      default:
        asrProvider = { type: "local_whisper" };
    }

    let llmProvider: LlmProvider;
    switch (llmType) {
      case "api":
        llmProvider = {
          type: "api",
          endpoint: llmEndpoint,
          api_key: llmApiKey,
          model: llmModel,
        };
        break;
      case "aws_bedrock":
        llmProvider = {
          type: "aws_bedrock",
          region: awsBedrockRegion,
          model_id: awsBedrockModelId,
          credential_source: buildAwsCredentialSource(
            awsBedrockCredentialMode,
            awsBedrockProfileName,
            awsBedrockAccessKey,
          ),
        };
        break;
      case "mistralrs":
        llmProvider = {
          type: "mistralrs",
          model_id: mistralrsModelId,
        };
        break;
      default:
        llmProvider = { type: "local_llama" };
    }

    const llmConfig: LlmApiConfig | null =
      llmType === "api" && llmEndpoint
        ? {
            endpoint: llmEndpoint,
            api_key: llmApiKey || null,
            model: llmModel,
            max_tokens: llmMaxTokens,
            temperature: llmTemperature,
          }
        : null;

    const geminiAuth: GeminiAuthMode =
      geminiAuthMode === "vertex_ai"
        ? {
            type: "vertex_ai",
            project_id: geminiProjectId,
            location: geminiLocation,
            ...(geminiServiceAccountPath
              ? { service_account_path: geminiServiceAccountPath }
              : {}),
          }
        : { type: "api_key", api_key: geminiApiKey };

    const gemini: GeminiSettings = {
      auth: geminiAuth,
      model: geminiModel,
    };

    await saveSettings({
      asr_provider: asrProvider,
      whisper_model: whisperModel,
      llm_provider: llmProvider,
      llm_api_config: llmConfig,
      audio_settings: settings?.audio_settings ?? {
        sample_rate: 16000,
        channels: 1,
      },
      gemini,
    });
  };

  const handleDeleteClick = (filename: string) => {
    if (confirmDelete === filename) {
      deleteModel(filename);
      setConfirmDelete(null);
    } else {
      setConfirmDelete(filename);
    }
  };

  // ── Render ────────────────────────────────────────────────────────────
  return (
    <div className="settings-overlay" onClick={closeSettings}>
      <div className="settings-modal" onClick={(e) => e.stopPropagation()}>
        {/* Header */}
        <div className="settings-header">
          <h2 className="settings-header__title">Settings</h2>
          <button
            className="settings-header__close"
            onClick={closeSettings}
            aria-label="Close settings"
          >
            ✕
          </button>
        </div>

        {settingsLoading ? (
          <div className="settings-content settings-content--loading">
            <p>Loading settings…</p>
          </div>
        ) : (
          <div className="settings-content">
            {/* ── Models Section ─────────────────────────────────── */}
            <div className="settings-section">
              <h3 className="settings-section__title">Models</h3>
              {models.map((model) => {
                const status =
                  modelStatus && model.name.toLowerCase().includes("whisper")
                    ? modelStatus.whisper
                    : modelStatus && model.name.toLowerCase().includes("sortformer")
                      ? modelStatus.sortformer
                      : modelStatus
                        ? modelStatus.llm
                        : ("NotDownloaded" as ModelReadiness);

                const badge = readinessBadge(status);
                const isThisDownloading =
                  isDownloading && downloadProgress?.model_name === model.name;
                const isThisDeleting = isDeletingModel === model.filename;

                return (
                  <div className="model-card" key={model.filename}>
                    <div className="model-card__header">
                      <div>
                        <span className="model-card__name">{model.name}</span>
                        <span className={`status-badge ${badge.cls}`}>
                          {badge.label}
                        </span>
                      </div>
                      <span className="model-card__size">
                        {formatSize(model.size_bytes)}
                      </span>
                    </div>
                    {model.description && (
                      <p className="model-card__description">
                        {model.description}
                      </p>
                    )}

                    <div className="model-card__actions">
                      {!model.is_downloaded && (
                        <button
                          className="settings-btn settings-btn--primary"
                          onClick={() => downloadModel(model.filename)}
                          disabled={isDownloading}
                        >
                          {isThisDownloading ? "Downloading…" : "Download"}
                        </button>
                      )}
                      {model.is_downloaded && (
                        <button
                          className="settings-btn settings-btn--danger"
                          onClick={() => handleDeleteClick(model.filename)}
                          disabled={isThisDeleting}
                        >
                          {isThisDeleting
                            ? "Deleting…"
                            : confirmDelete === model.filename
                              ? "Confirm Delete"
                              : "Delete"}
                        </button>
                      )}
                    </div>

                    {/* Download progress bar */}
                    {isThisDownloading && downloadProgress && (
                      <div className="download-progress">
                        <div
                          className="download-progress__bar"
                          style={{ width: `${downloadProgress.percent}%` }}
                        />
                      </div>
                    )}
                  </div>
                );
              })}
              {models.length === 0 && (
                <p className="settings-section__empty">No models available.</p>
              )}
            </div>

            {/* ── ASR Provider Section ───────────────────────────── */}
            <div className="settings-section">
              <h3 className="settings-section__title">ASR Provider</h3>
              <div className="settings-radio-group">
                <label className="settings-radio">
                  <input
                    type="radio"
                    name="asr-provider"
                    checked={asrType === "local_whisper"}
                    onChange={() => setAsrType("local_whisper")}
                  />
                  <span>Local Whisper</span>
                  {asrType === "local_whisper" && modelStatus && (
                    <span
                      className={`status-badge ${readinessBadge(modelStatus.whisper).cls}`}
                    >
                      {readinessBadge(modelStatus.whisper).label}
                    </span>
                  )}
                </label>

              {asrType === "local_whisper" && (
                <div className="settings-section__api-fields">
                  <div className="settings-field">
                    <label className="settings-field__label">Whisper Model Size</label>
                    <select
                      className="settings-input"
                      value={whisperModel}
                      onChange={(e) => setWhisperModel(e.target.value)}
                    >
                      <option value="ggml-tiny.en.bin">Tiny (~75MB) - Fastest, lower accuracy</option>
                      <option value="ggml-base.en.bin">Base (~142MB) - Best real-time balance</option>
                      <option value="ggml-small.en.bin">Small (~466MB) - Default, good accuracy</option>
                      <option value="ggml-medium.en.bin">Medium (~1.5GB) - High accuracy, needs GPU</option>
                      <option value="ggml-large-v3.bin">Large v3 (~3GB) - Best, multilingual</option>
                    </select>
                  </div>
                </div>
              )}

                <label className="settings-radio">
                  <input
                    type="radio"
                    name="asr-provider"
                    checked={asrType === "api"}
                    onChange={() => setAsrType("api")}
                  />
                  <span>Cloud API (Groq/OpenAI)</span>
                </label>
                <label className="settings-radio">
                  <input
                    type="radio"
                    name="asr-provider"
                    checked={asrType === "aws_transcribe"}
                    onChange={() => setAsrType("aws_transcribe")}
                  />
                  <span>AWS Transcribe</span>
                </label>
                <label className="settings-radio">
                  <input
                    type="radio"
                    name="asr-provider"
                    checked={asrType === "deepgram"}
                    onChange={() => setAsrType("deepgram")}
                  />
                  <span>Deepgram</span>
                </label>
                <label className="settings-radio">
                  <input
                    type="radio"
                    name="asr-provider"
                    checked={asrType === "assemblyai"}
                    onChange={() => setAsrType("assemblyai")}
                  />
                  <span>AssemblyAI</span>
                </label>
                <label className="settings-radio">
                  <input
                    type="radio"
                    name="asr-provider"
                    checked={asrType === "sherpa_onnx"}
                    onChange={() => setAsrType("sherpa_onnx")}
                  />
                  <span>Sherpa-ONNX (Local Streaming)</span>
                </label>
              </div>

              {asrType === "api" && (
                <div className="settings-section__api-fields">
                  <div className="settings-field">
                    <label className="settings-field__label">
                      Endpoint URL
                    </label>
                    <input
                      className="settings-input"
                      type="text"
                      value={asrEndpoint}
                      onChange={(e) => setAsrEndpoint(e.target.value)}
                      placeholder="https://api.openai.com/v1"
                    />
                  </div>
                  <div className="settings-field">
                    <label className="settings-field__label">API Key</label>
                    <input
                      className="settings-input"
                      type="password"
                      value={asrApiKey}
                      onChange={(e) => setAsrApiKey(e.target.value)}
                      placeholder="sk-..."
                    />
                  </div>
                  <div className="settings-field">
                    <label className="settings-field__label">Model</label>
                    <input
                      className="settings-input"
                      type="text"
                      value={asrModel}
                      onChange={(e) => setAsrModel(e.target.value)}
                      placeholder="whisper-1"
                    />
                  </div>
                </div>
              )}

              {asrType === "aws_transcribe" && (
                <div className="settings-section__api-fields">
                  <div className="settings-field">
                    <label className="settings-field__label">Region</label>
                    <input
                      className="settings-input"
                      type="text"
                      value={awsAsrRegion}
                      onChange={(e) => setAwsAsrRegion(e.target.value)}
                      placeholder="us-east-1"
                    />
                  </div>
                  <div className="settings-field">
                    <label className="settings-field__label">
                      Language Code
                    </label>
                    <input
                      className="settings-input"
                      type="text"
                      value={awsAsrLanguageCode}
                      onChange={(e) => setAwsAsrLanguageCode(e.target.value)}
                      placeholder="en-US"
                    />
                  </div>
                  <div className="settings-field">
                    <label className="settings-field__label">
                      Credential Mode
                    </label>
                    <select
                      className="settings-input"
                      value={awsAsrCredentialMode}
                      onChange={(e) =>
                        setAwsAsrCredentialMode(
                          e.target.value as
                            | "default_chain"
                            | "profile"
                            | "access_keys",
                        )
                      }
                    >
                      <option value="default_chain">Default Chain</option>
                      <option value="profile">Profile</option>
                      <option value="access_keys">Access Keys</option>
                    </select>
                  </div>
                  {awsAsrCredentialMode === "profile" && (
                    <div className="settings-field">
                      <label className="settings-field__label">
                        Profile Name
                      </label>
                      <input
                        className="settings-input"
                        type="text"
                        value={awsAsrProfileName}
                        onChange={(e) => setAwsAsrProfileName(e.target.value)}
                        placeholder="default"
                      />
                    </div>
                  )}
                  {awsAsrCredentialMode === "access_keys" && (
                    <div className="settings-field">
                      <label className="settings-field__label">
                        Access Key
                      </label>
                      <input
                        className="settings-input"
                        type="password"
                        value={awsAsrAccessKey}
                        onChange={(e) => setAwsAsrAccessKey(e.target.value)}
                        placeholder="AKIA..."
                      />
                    </div>
                  )}
                  <div className="settings-field">
                    <label className="settings-radio">
                      <input
                        type="checkbox"
                        checked={awsAsrDiarization}
                        onChange={(e) => setAwsAsrDiarization(e.target.checked)}
                      />
                      <span>Enable Diarization</span>
                    </label>
                  </div>
                </div>
              )}

              {asrType === "deepgram" && (
                <div className="settings-section__api-fields">
                  <div className="settings-field">
                    <label className="settings-field__label">API Key</label>
                    <input
                      className="settings-input"
                      type="password"
                      value={deepgramApiKey}
                      onChange={(e) => setDeepgramApiKey(e.target.value)}
                      placeholder="dg-..."
                    />
                  </div>
                  <div className="settings-field">
                    <label className="settings-field__label">Model</label>
                    <input
                      className="settings-input"
                      type="text"
                      value={deepgramModel}
                      onChange={(e) => setDeepgramModel(e.target.value)}
                      placeholder="nova-3"
                    />
                  </div>
                  <div className="settings-field">
                    <label className="settings-radio">
                      <input
                        type="checkbox"
                        checked={deepgramDiarization}
                        onChange={(e) =>
                          setDeepgramDiarization(e.target.checked)
                        }
                      />
                      <span>Enable Diarization</span>
                    </label>
                  </div>
                </div>
              )}

              {asrType === "assemblyai" && (
                <div className="settings-section__api-fields">
                  <div className="settings-field">
                    <label className="settings-field__label">API Key</label>
                    <input
                      className="settings-input"
                      type="password"
                      value={assemblyaiApiKey}
                      onChange={(e) => setAssemblyaiApiKey(e.target.value)}
                      placeholder="AssemblyAI API key"
                    />
                  </div>
                  <div className="settings-field">
                    <label className="settings-radio">
                      <input
                        type="checkbox"
                        checked={assemblyaiDiarization}
                        onChange={(e) =>
                          setAssemblyaiDiarization(e.target.checked)
                        }
                      />
                      <span>Enable Diarization</span>
                    </label>
                  </div>
                </div>
              )}

              {asrType === "sherpa_onnx" && (
                <div className="settings-section__api-fields">
                  <div className="settings-field">
                    <label className="settings-field__label">Model Directory</label>
                    <input
                      className="settings-input"
                      type="text"
                      value={sherpaModelDir}
                      onChange={(e) => setSherpaModelDir(e.target.value)}
                      placeholder="streaming-zipformer-en-20M"
                    />
                  </div>
                  <div className="settings-field">
                    <label className="settings-radio">
                      <input
                        type="checkbox"
                        checked={sherpaEndpointDetection}
                        onChange={(e) => setSherpaEndpointDetection(e.target.checked)}
                      />
                      <span>Enable endpoint detection</span>
                    </label>
                  </div>
                </div>
              )}
            </div>

            {/* ── LLM Provider Section ───────────────────────────── */}
            <div className="settings-section">
              <h3 className="settings-section__title">LLM Provider</h3>
              <div className="settings-radio-group">
                <label className="settings-radio">
                  <input
                    type="radio"
                    name="llm-provider"
                    checked={llmType === "local_llama"}
                    onChange={() => setLlmType("local_llama")}
                  />
                  <span>Local LLM (LFM2-350M)</span>
                  {llmType === "local_llama" && modelStatus && (
                    <span
                      className={`status-badge ${readinessBadge(modelStatus.llm).cls}`}
                    >
                      {readinessBadge(modelStatus.llm).label}
                    </span>
                  )}
                </label>
                <label className="settings-radio">
                  <input
                    type="radio"
                    name="llm-provider"
                    checked={llmType === "api"}
                    onChange={() => setLlmType("api")}
                  />
                  <span>OpenAI-compatible API</span>
                </label>
                <label className="settings-radio">
                  <input
                    type="radio"
                    name="llm-provider"
                    checked={llmType === "aws_bedrock"}
                    onChange={() => setLlmType("aws_bedrock")}
                  />
                  <span>AWS Bedrock</span>
                </label>
                <label className="settings-radio">
                  <input
                    type="radio"
                    name="llm-provider"
                    checked={llmType === "mistralrs"}
                    onChange={() => setLlmType("mistralrs")}
                  />
                  <span>Mistral.rs (Local Candle)</span>
                </label>
              </div>

              {llmType === "api" && (
                <div className="settings-section__api-fields">
                  <div className="settings-field">
                    <label className="settings-field__label">
                      Endpoint URL
                    </label>
                    <input
                      className="settings-input"
                      type="text"
                      value={llmEndpoint}
                      onChange={(e) => setLlmEndpoint(e.target.value)}
                      placeholder="https://openrouter.ai/api/v1"
                    />
                  </div>
                  <div className="settings-field">
                    <label className="settings-field__label">API Key</label>
                    <input
                      className="settings-input"
                      type="password"
                      value={llmApiKey}
                      onChange={(e) => setLlmApiKey(e.target.value)}
                      placeholder="sk-..."
                    />
                  </div>
                  <div className="settings-field">
                    <label className="settings-field__label">Model</label>
                    <input
                      className="settings-input"
                      type="text"
                      value={llmModel}
                      onChange={(e) => setLlmModel(e.target.value)}
                      placeholder="gpt-4o-mini"
                    />
                  </div>
                  <div className="settings-field">
                    <label className="settings-field__label">
                      Max Tokens ({llmMaxTokens})
                    </label>
                    <input
                      className="settings-input"
                      type="number"
                      value={llmMaxTokens}
                      onChange={(e) => setLlmMaxTokens(Number(e.target.value))}
                      min={1}
                      max={32768}
                    />
                  </div>
                  <div className="settings-field">
                    <label className="settings-field__label">
                      Temperature ({llmTemperature})
                    </label>
                    <input
                      className="settings-input"
                      type="number"
                      step="0.1"
                      value={llmTemperature}
                      onChange={(e) =>
                        setLlmTemperature(Number(e.target.value))
                      }
                      min={0}
                      max={2}
                    />
                  </div>
                </div>
              )}

              {llmType === "aws_bedrock" && (
                <div className="settings-section__api-fields">
                  <div className="settings-field">
                    <label className="settings-field__label">Region</label>
                    <input
                      className="settings-input"
                      type="text"
                      value={awsBedrockRegion}
                      onChange={(e) => setAwsBedrockRegion(e.target.value)}
                      placeholder="us-east-1"
                    />
                  </div>
                  <div className="settings-field">
                    <label className="settings-field__label">Model ID</label>
                    <input
                      className="settings-input"
                      type="text"
                      value={awsBedrockModelId}
                      onChange={(e) => setAwsBedrockModelId(e.target.value)}
                      placeholder="anthropic.claude-3-haiku-20240307-v1:0"
                    />
                  </div>
                  <div className="settings-field">
                    <label className="settings-field__label">
                      Credential Mode
                    </label>
                    <select
                      className="settings-input"
                      value={awsBedrockCredentialMode}
                      onChange={(e) =>
                        setAwsBedrockCredentialMode(
                          e.target.value as
                            | "default_chain"
                            | "profile"
                            | "access_keys",
                        )
                      }
                    >
                      <option value="default_chain">Default Chain</option>
                      <option value="profile">Profile</option>
                      <option value="access_keys">Access Keys</option>
                    </select>
                  </div>
                  {awsBedrockCredentialMode === "profile" && (
                    <div className="settings-field">
                      <label className="settings-field__label">
                        Profile Name
                      </label>
                      <input
                        className="settings-input"
                        type="text"
                        value={awsBedrockProfileName}
                        onChange={(e) =>
                          setAwsBedrockProfileName(e.target.value)
                        }
                        placeholder="default"
                      />
                    </div>
                  )}
                  {awsBedrockCredentialMode === "access_keys" && (
                    <div className="settings-field">
                      <label className="settings-field__label">
                        Access Key
                      </label>
                      <input
                        className="settings-input"
                        type="password"
                        value={awsBedrockAccessKey}
                        onChange={(e) =>
                          setAwsBedrockAccessKey(e.target.value)
                        }
                        placeholder="AKIA..."
                      />
                    </div>
                  )}
                </div>
              )}

              {llmType === "mistralrs" && (
                <div className="settings-section__api-fields">
                  <div className="settings-field">
                    <label className="settings-field__label">Model ID</label>
                    <input
                      className="settings-input"
                      type="text"
                      value={mistralrsModelId}
                      onChange={(e) => setMistralrsModelId(e.target.value)}
                      placeholder="ggml-small-extract.gguf"
                    />
                  </div>
                </div>
              )}
            </div>

            {/* ── Gemini Live Section ──────────────────────────── */}
            <div className="settings-section">
              <h3 className="settings-section__title">Gemini Live</h3>
              <div className="settings-radio-group">
                <label className="settings-radio">
                  <input
                    type="radio"
                    name="gemini-auth"
                    checked={geminiAuthMode === "api_key"}
                    onChange={() => setGeminiAuthMode("api_key")}
                  />
                  <span>AI Studio (API Key)</span>
                </label>
                <label className="settings-radio">
                  <input
                    type="radio"
                    name="gemini-auth"
                    checked={geminiAuthMode === "vertex_ai"}
                    onChange={() => setGeminiAuthMode("vertex_ai")}
                  />
                  <span>Vertex AI</span>
                </label>
              </div>

              <div className="settings-section__api-fields">
                {geminiAuthMode === "api_key" && (
                  <div className="settings-field">
                    <label className="settings-field__label">
                      Gemini API Key
                    </label>
                    <input
                      className="settings-input"
                      type="password"
                      value={geminiApiKey}
                      onChange={(e) => setGeminiApiKey(e.target.value)}
                      placeholder="AIza..."
                    />
                  </div>
                )}

                {geminiAuthMode === "vertex_ai" && (
                  <>
                    <div className="settings-field">
                      <label className="settings-field__label">
                        Project ID
                      </label>
                      <input
                        className="settings-input"
                        type="text"
                        value={geminiProjectId}
                        onChange={(e) => setGeminiProjectId(e.target.value)}
                        placeholder="my-gcp-project"
                      />
                    </div>
                    <div className="settings-field">
                      <label className="settings-field__label">Location</label>
                      <input
                        className="settings-input"
                        type="text"
                        value={geminiLocation}
                        onChange={(e) => setGeminiLocation(e.target.value)}
                        placeholder="us-central1"
                      />
                    </div>
                    <div className="settings-field">
                      <label className="settings-field__label">
                        Service Account Path (optional)
                      </label>
                      <input
                        className="settings-input"
                        type="text"
                        value={geminiServiceAccountPath}
                        onChange={(e) =>
                          setGeminiServiceAccountPath(e.target.value)
                        }
                        placeholder="/path/to/service-account.json"
                      />
                    </div>
                  </>
                )}

                <div className="settings-field">
                  <label className="settings-field__label">Model</label>
                  <input
                    className="settings-input"
                    type="text"
                    value={geminiModel}
                    onChange={(e) => setGeminiModel(e.target.value)}
                    placeholder="gemini-3.1-flash-live-preview"
                  />
                </div>
              </div>
            </div>
          </div>
        )}

        {/* Footer */}
        <div className="settings-footer">
          <button
            className="settings-btn settings-btn--primary"
            onClick={handleSave}
            disabled={settingsLoading}
          >
            Save Settings
          </button>
        </div>
      </div>
    </div>
  );
}

export default SettingsPage;
