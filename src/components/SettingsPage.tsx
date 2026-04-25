/**
 * Settings modal — the full configuration surface for the app.
 *
 * Composes five sub-forms (`AudioSettings`, `AsrProviderSettings`,
 * `LlmProviderSettings`, `GeminiSettings`, `CredentialsManager`) around a
 * shared `useReducer`-based form state (see `settingsTypes.ts`). The
 * reducer lives in this component so every sub-form dispatches against
 * the same snapshot; the top-level "Save" button invokes
 * `save_settings_cmd` once with the full patched `AppSettings`.
 *
 * Focus is trapped in the modal via `useFocusTrap` and release on unmount.
 * Escape is handled by `useKeyboardShortcuts` at the App level.
 *
 * Store bindings: `settings` (seed), `loadSettings`, `settingsOpen`,
 * `closeSettings` — `openSettings` is invoked from `ControlBar` /
 * `App.tsx` keyboard handler / `ExpressSetup` Advanced link.
 *
 * Parent: `App.tsx` (rendered conditionally when `settingsOpen` is true).
 * No props.
 */
import { useEffect, useReducer } from "react";
import { useTranslation } from "react-i18next";
import { invoke } from "@tauri-apps/api/core";
import { useAudioGraphStore } from "../store";
import { useFocusTrap } from "../hooks/useFocusTrap";
import type {
  AsrProvider,
  GeminiAuthMode,
  GeminiSettings as GeminiSettingsType,
  LlmApiConfig,
  LlmProvider,
} from "../types";
import {
  buildAwsCredentialSource,
  initialSettingsState,
  setField,
  settingsReducer,
  type ChannelCount,
  type LogLevel,
  type SampleRate,
  type SettingsState,
  type TestKey,
} from "./settingsTypes";
import AudioSettings from "./AudioSettings";
import AsrProviderSettings from "./AsrProviderSettings";
import LlmProviderSettings from "./LlmProviderSettings";
import GeminiSettings from "./GeminiSettings";
import CredentialsManager from "./CredentialsManager";

function SettingsPage() {
  const { t } = useTranslation();
  const modalRef = useFocusTrap<HTMLDivElement>();
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
    listAwsProfiles,
  } = useAudioGraphStore();

  const [state, dispatch] = useReducer(settingsReducer, initialSettingsState);
  const {
    asrType,
    whisperModel,
    asrEndpoint,
    asrApiKey,
    asrModel,
    awsAsrRegion,
    awsAsrLanguageCode,
    awsAsrCredentialMode,
    awsAsrProfileName,
    awsAsrAccessKey,
    awsAsrSecretKey,
    awsAsrSessionToken,
    awsAsrDiarization,
    deepgramApiKey,
    deepgramModel,
    deepgramDiarization,
    assemblyaiApiKey,
    assemblyaiDiarization,
    sherpaModelDir,
    sherpaEndpointDetection,
    llmType,
    llmEndpoint,
    llmApiKey,
    llmModel,
    llmMaxTokens,
    llmTemperature,
    mistralrsModelId,
    awsBedrockRegion,
    awsBedrockModelId,
    awsBedrockCredentialMode,
    awsBedrockProfileName,
    awsBedrockAccessKey,
    awsBedrockSecretKey,
    awsBedrockSessionToken,
    geminiAuthMode,
    geminiApiKey,
    geminiModel,
    geminiProjectId,
    geminiLocation,
    geminiServiceAccountPath,
    audioSampleRate,
    audioChannels,
    logLevel,
    confirmDelete,
    testResults,
    testingKey,
  } = state;

  const refreshAwsProfiles = async () => {
    dispatch({ type: "SET_AWS_PROFILES", profiles: await listAwsProfiles() });
  };

  // Upper bound on any Test Connection invocation. Without this, a hung
  // network call (e.g. provider stuck in TLS handshake, firewall silently
  // dropping packets) leaves the button forever stuck on "Testing…".
  const TEST_TIMEOUT_MS = 10_000;

  const runTest = async (
    key: TestKey,
    invocation: () => Promise<string>,
  ) => {
    // Debounce: reject rapid re-clicks while a test is already in flight.
    if (testingKey !== null) return;
    dispatch({ type: "TEST_START", key });
    try {
      const msg = await Promise.race([
        invocation(),
        new Promise<never>((_, reject) =>
          setTimeout(
            () =>
              reject(
                new Error(
                  t("settings.errors.testTimeout", {
                    seconds: TEST_TIMEOUT_MS / 1000,
                  }),
                ),
              ),
            TEST_TIMEOUT_MS,
          ),
        ),
      ]);
      dispatch({ type: "TEST_RESULT", key, result: { ok: true, msg } });
    } catch (e) {
      dispatch({
        type: "TEST_RESULT",
        key,
        result: { ok: false, msg: String(e) },
      });
    } finally {
      dispatch({ type: "TEST_FINISH" });
    }
  };

  // Clear a stored credential (mirrors the Rust `delete_credential` path).
  const handleClearCredential = async (
    key: string,
    label: string,
    clearLocal: () => void,
  ) => {
    const ok = window.confirm(
      t("settings.credentialConfirm.clearPrompt", { label }),
    );
    if (!ok) return;
    try {
      await invoke("delete_credential_cmd", { key });
      clearLocal();
    } catch (e) {
      console.error(`Failed to clear ${key}:`, e);
      window.alert(t("settings.errors.failedToClear", { error: String(e) }));
    }
  };

  const handleTestAsrApi = () =>
    runTest("asr_api", () =>
      invoke<string>("test_cloud_asr_connection", {
        endpoint: asrEndpoint,
        apiKey: asrApiKey,
      }),
    );

  const handleTestDeepgram = () =>
    runTest("deepgram", () =>
      invoke<string>("test_deepgram_connection", { apiKey: deepgramApiKey }),
    );

  const handleTestAssemblyAI = () =>
    runTest("assemblyai", () =>
      invoke<string>("test_assemblyai_connection", { apiKey: assemblyaiApiKey }),
    );

  const handleTestGemini = () =>
    runTest("gemini", () =>
      invoke<string>("test_gemini_api_key", { apiKey: geminiApiKey }),
    );

  const handleTestAwsAsr = async () => {
    // If user is in access_keys mode, persist the secret + session to the
    // credential store first so the backend `test_aws_credentials` command
    // (which reads from credentials.yaml) can see them.
    if (awsAsrCredentialMode === "access_keys") {
      try {
        if (awsAsrSecretKey) {
          await invoke("save_credential_cmd", {
            key: "aws_secret_key",
            value: awsAsrSecretKey,
          });
        }
        if (awsAsrSessionToken) {
          await invoke("save_credential_cmd", {
            key: "aws_session_token",
            value: awsAsrSessionToken,
          });
        }
      } catch (e) {
        console.error("Failed to stage AWS credentials before test:", e);
      }
    }
    const credential_source = buildAwsCredentialSource(
      awsAsrCredentialMode,
      awsAsrProfileName,
      awsAsrAccessKey,
    );
    return runTest("aws_asr", () =>
      invoke<string>("test_aws_credentials", {
        region: awsAsrRegion,
        credentialSource: credential_source,
      }),
    );
  };

  const handleTestAwsBedrock = async () => {
    if (awsBedrockCredentialMode === "access_keys") {
      try {
        if (awsBedrockSecretKey) {
          await invoke("save_credential_cmd", {
            key: "aws_secret_key",
            value: awsBedrockSecretKey,
          });
        }
        if (awsBedrockSessionToken) {
          await invoke("save_credential_cmd", {
            key: "aws_session_token",
            value: awsBedrockSessionToken,
          });
        }
      } catch (e) {
        console.error("Failed to stage AWS credentials before test:", e);
      }
    }
    const credential_source = buildAwsCredentialSource(
      awsBedrockCredentialMode,
      awsBedrockProfileName,
      awsBedrockAccessKey,
    );
    return runTest("aws_bedrock", () =>
      invoke<string>("test_aws_credentials", {
        region: awsBedrockRegion,
        credentialSource: credential_source,
      }),
    );
  };

  /** Render a test result line (green/red) for a given provider key. */
  const renderTestResult = (key: TestKey) => {
    const r = testResults[key];
    if (!r) return null;
    return (
      <div className={r.ok ? "settings-test-ok" : "settings-test-err"}>
        {r.ok ? "✓ " : "✗ "}
        {r.msg}
      </div>
    );
  };

  // Sync local state when settings are loaded
  useEffect(() => {
    if (!settings) return;

    // Audio capture format — clamp to the UI whitelist so an out-of-band
    // value from a hand-edited settings.json doesn't leave the dropdown
    // in a "Custom (n/a)" state. The backend does the same fallback in
    // `resolve_audio_settings`.
    const ALLOWED_RATES: SampleRate[] = [16000, 22050, 44100, 48000, 88200, 96000];
    const ALLOWED_CHANNELS: ChannelCount[] = [1, 2];
    const sr = settings.audio_settings?.sample_rate;
    const ch = settings.audio_settings?.channels;
    const patch: Partial<SettingsState> = {
      audioSampleRate: ALLOWED_RATES.includes(sr as SampleRate)
        ? (sr as SampleRate)
        : 16000,
      audioChannels: ALLOWED_CHANNELS.includes(ch as ChannelCount)
        ? (ch as ChannelCount)
        : 1,
    };

    // Whisper model selection
    if (settings.whisper_model) {
      patch.whisperModel = settings.whisper_model;
    }

    // ASR provider
    const asr = settings.asr_provider;
    patch.asrType = asr.type;
    if (asr.type === "api") {
      patch.asrEndpoint = asr.endpoint;
      patch.asrApiKey = asr.api_key;
      patch.asrModel = asr.model;
    } else if (asr.type === "aws_transcribe") {
      patch.awsAsrRegion = asr.region;
      patch.awsAsrLanguageCode = asr.language_code;
      patch.awsAsrDiarization = asr.enable_diarization;
      const cred = asr.credential_source;
      patch.awsAsrCredentialMode = cred.type;
      if (cred.type === "profile") patch.awsAsrProfileName = cred.name;
      if (cred.type === "access_keys") patch.awsAsrAccessKey = cred.access_key;
    } else if (asr.type === "deepgram") {
      patch.deepgramApiKey = asr.api_key;
      patch.deepgramModel = asr.model;
      patch.deepgramDiarization = asr.enable_diarization;
    } else if (asr.type === "assemblyai") {
      patch.assemblyaiApiKey = asr.api_key;
      patch.assemblyaiDiarization = asr.enable_diarization;
    } else if (asr.type === "sherpa_onnx") {
      patch.sherpaModelDir = asr.model_dir;
      patch.sherpaEndpointDetection = asr.enable_endpoint_detection;
    }

    // LLM provider
    const llm = settings.llm_provider;
    patch.llmType = llm.type;
    if (llm.type === "api") {
      patch.llmEndpoint = llm.endpoint;
      patch.llmApiKey = llm.api_key;
      patch.llmModel = llm.model;
    } else if (llm.type === "aws_bedrock") {
      patch.awsBedrockRegion = llm.region;
      patch.awsBedrockModelId = llm.model_id;
      const cred = llm.credential_source;
      patch.awsBedrockCredentialMode = cred.type;
      if (cred.type === "profile") patch.awsBedrockProfileName = cred.name;
      if (cred.type === "access_keys")
        patch.awsBedrockAccessKey = cred.access_key;
    } else if (llm.type === "mistralrs") {
      patch.mistralrsModelId = llm.model_id;
    }

    // LLM config (advanced — max_tokens / temperature)
    if (settings.llm_api_config) {
      patch.llmMaxTokens = settings.llm_api_config.max_tokens;
      patch.llmTemperature = settings.llm_api_config.temperature;
    }

    // Diagnostics: log level — default to "info" if missing or malformed so
    // the dropdown always has a legitimate selection.
    const LOG_LEVELS: LogLevel[] = [
      "off",
      "error",
      "warn",
      "info",
      "debug",
      "trace",
    ];
    const raw = (settings.log_level ?? "info").toLowerCase() as LogLevel;
    patch.logLevel = LOG_LEVELS.includes(raw) ? raw : "info";

    // Gemini settings
    if (settings.gemini) {
      patch.geminiModel = settings.gemini.model;
      const auth = settings.gemini.auth;
      patch.geminiAuthMode = auth.type;
      if (auth.type === "api_key") {
        patch.geminiApiKey = auth.api_key;
      } else if (auth.type === "vertex_ai") {
        patch.geminiProjectId = auth.project_id;
        patch.geminiLocation = auth.location;
        patch.geminiServiceAccountPath = auth.service_account_path ?? "";
      }
    }

    dispatch({ type: "HYDRATE_FROM_SETTINGS", patch });

    // Pre-populate AWS secret key + session token from credentials.yaml.
    // Both AWS ASR and AWS Bedrock share the same aws_secret_key / aws_session_token
    // in the backend credential store, so we load once and mirror into both forms.
    (async () => {
      try {
        const secret = await invoke<string | null>("load_credential_cmd", {
          key: "aws_secret_key",
        });
        if (secret) {
          dispatch({ type: "SET_AWS_SHARED_SECRET", secret });
        }
      } catch {
        // Silently tolerate missing credentials.
      }
      try {
        const token = await invoke<string | null>("load_credential_cmd", {
          key: "aws_session_token",
        });
        if (token) {
          dispatch({ type: "SET_AWS_SHARED_SESSION_TOKEN", token });
        }
      } catch {
        // Silently tolerate missing credentials.
      }
    })();
  }, [settings]);

  // Fetch AWS profiles whenever settings load or the user switches an AWS
  // section into "profile" credential mode. Cheap Tauri call — just parses
  // two small files — so it's fine to re-run on mode change.
  useEffect(() => {
    if (!settings) return;
    if (
      awsAsrCredentialMode === "profile" ||
      awsBedrockCredentialMode === "profile"
    ) {
      refreshAwsProfiles();
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [settings, awsAsrCredentialMode, awsBedrockCredentialMode]);

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

    const gemini: GeminiSettingsType = {
      auth: geminiAuth,
      model: geminiModel,
    };

    await saveSettings({
      asr_provider: asrProvider,
      whisper_model: whisperModel,
      llm_provider: llmProvider,
      llm_api_config: llmConfig,
      audio_settings: {
        sample_rate: audioSampleRate,
        channels: audioChannels,
      },
      gemini,
      log_level: logLevel,
      // Preserve the stored demo-mode decision across a Settings save.
      // The settings page itself has no UI for this field; dropping it
      // would regress to `undefined` and cause the backend to re-run the
      // first-launch decision on next boot.
      demo_mode: settings?.demo_mode,
    });

    // Persist AWS secret key + session token to credentials.yaml when the user
    // is using access_keys mode. ASR and Bedrock share the same credential
    // entries in the backend, so we prefer whichever form the user actually
    // filled in (ASR first, then Bedrock as fallback). We NEVER overwrite
    // stored credentials with empty strings — that would silently wipe them.
    const usingAwsAsrKeys =
      asrType === "aws_transcribe" && awsAsrCredentialMode === "access_keys";
    const usingAwsBedrockKeys =
      llmType === "aws_bedrock" && awsBedrockCredentialMode === "access_keys";

    if (usingAwsAsrKeys || usingAwsBedrockKeys) {
      const secretCandidate =
        (usingAwsAsrKeys && awsAsrSecretKey) ||
        (usingAwsBedrockKeys && awsBedrockSecretKey) ||
        "";
      if (secretCandidate) {
        try {
          await invoke("save_credential_cmd", {
            key: "aws_secret_key",
            value: secretCandidate,
          });
        } catch (e) {
          console.error("Failed to save aws_secret_key:", e);
        }
      }

      const sessionCandidate =
        (usingAwsAsrKeys && awsAsrSessionToken) ||
        (usingAwsBedrockKeys && awsBedrockSessionToken) ||
        "";
      if (sessionCandidate) {
        try {
          await invoke("save_credential_cmd", {
            key: "aws_session_token",
            value: sessionCandidate,
          });
        } catch (e) {
          console.error("Failed to save aws_session_token:", e);
        }
      }
    }
  };

  // Apply a log-level change immediately (takes effect for every subsequent
  // `log::*!` macro on the backend) AND kick off persistence so it survives
  // restart. We intentionally call the dedicated command rather than relying
  // on the user clicking Save — a verbosity change is most useful *now*.
  const handleLogLevelChange = async (next: LogLevel) => {
    dispatch(setField("logLevel", next));
    try {
      await invoke("set_log_level", { level: next });
    } catch (e) {
      console.error("Failed to set log level:", e);
    }
  };

  const handleDeleteClick = (filename: string) => {
    if (confirmDelete === filename) {
      deleteModel(filename);
      dispatch({ type: "SET_CONFIRM_DELETE", filename: null });
    } else {
      dispatch({ type: "SET_CONFIRM_DELETE", filename });
    }
  };

  // ── Render ────────────────────────────────────────────────────────────
  return (
    <div className="settings-overlay" onClick={closeSettings}>
      <div
        ref={modalRef}
        className="settings-modal"
        onClick={(e) => e.stopPropagation()}
        role="dialog"
        aria-modal="true"
        aria-labelledby="settings-header-title"
        tabIndex={-1}
      >
        {/* Header */}
        <div className="settings-header">
          <h2
            id="settings-header-title"
            className="settings-header__title"
          >
            {t("settings.title")}
          </h2>
          <button
            className="settings-header__close"
            onClick={closeSettings}
            aria-label={t("settings.close")}
          >
            ✕
          </button>
        </div>

        {settingsLoading ? (
          <div className="settings-content settings-content--loading">
            <p>{t("settings.loading")}</p>
          </div>
        ) : (
          <div className="settings-content">
            <AudioSettings state={state} dispatch={dispatch} t={t} />
            <CredentialsManager
              state={state}
              t={t}
              models={models}
              modelStatus={modelStatus}
              isDownloading={isDownloading}
              isDeletingModel={isDeletingModel}
              downloadProgress={downloadProgress}
              downloadModel={downloadModel}
              handleDeleteClick={handleDeleteClick}
              handleLogLevelChange={handleLogLevelChange}
            />
            <AsrProviderSettings
              state={state}
              dispatch={dispatch}
              t={t}
              modelStatus={modelStatus}
              refreshAwsProfiles={refreshAwsProfiles}
              handleTestAsrApi={handleTestAsrApi}
              handleTestDeepgram={handleTestDeepgram}
              handleTestAssemblyAI={handleTestAssemblyAI}
              handleTestAwsAsr={handleTestAwsAsr}
              handleClearCredential={handleClearCredential}
              renderTestResult={renderTestResult}
            />
            <LlmProviderSettings
              state={state}
              dispatch={dispatch}
              t={t}
              modelStatus={modelStatus}
              refreshAwsProfiles={refreshAwsProfiles}
              handleTestAwsBedrock={handleTestAwsBedrock}
              handleClearCredential={handleClearCredential}
              renderTestResult={renderTestResult}
            />
            <GeminiSettings
              state={state}
              dispatch={dispatch}
              t={t}
              handleTestGemini={handleTestGemini}
              renderTestResult={renderTestResult}
            />
          </div>
        )}

        {/* Footer */}
        <div className="settings-footer">
          <button
            className="settings-btn settings-btn--primary"
            onClick={handleSave}
            disabled={settingsLoading}
          >
            {t("settings.buttons.save")}
          </button>
        </div>
      </div>
    </div>
  );
}

export default SettingsPage;
