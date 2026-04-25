/**
 * Credentials manager — sub-form in `SettingsPage` for editing provider
 * API keys and related credential state (AWS profile/region, Google
 * service account path, etc.).
 *
 * This file actually composes three surfaces:
 *   1. A row per allow-listed credential key with show/hide, save,
 *      delete, and (where applicable) "Test connection" controls that
 *      invoke the provider-specific test commands
 *      (`test_cloud_asr_connection`, `test_deepgram_connection`,
 *      `test_assemblyai_connection`, `test_gemini_api_key`,
 *      `test_aws_credentials`).
 *   2. A log-level switch (persisted via `save_settings_cmd` +
 *      `set_log_level`).
 *   3. A models-readiness panel (Whisper / llama / sortformer) showing
 *      `ModelStatus` badges from the store.
 *
 * Secrets live in-memory as plain strings while the form is open, but
 * are zeroized on the Rust side once `save_credential_cmd` writes them
 * to `credentials.yaml`. The allow-list is kept consistent via
 * `ALLOWED_CREDENTIAL_KEYS` in both `src/types/index.ts` and
 * `src-tauri/src/credentials/mod.rs`.
 *
 * Parent: `SettingsPage.tsx`. Props are the reducer `state` / `dispatch`
 * + translation handle; see the inline type below.
 */
import type { TFunction } from "i18next";
import {
  readinessBadge,
  type LogLevel,
  type SettingsState,
} from "./settingsTypes";
import type {
  DownloadProgress,
  ModelInfo,
  ModelReadiness,
  ModelStatus,
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

/** Compact "MB" string used inside progress lines (always shows the unit). */
function formatDownloadedMB(bytes: number): string {
  const mb = bytes / (1024 * 1024);
  if (mb >= 1024) {
    return `${(mb / 1024).toFixed(1)} GB`;
  }
  return `${Math.round(mb)} MB`;
}

/**
 * Format a remaining-time estimate as `Xs`, `Xm Ys`, or `Xh Ym`. We prefer a
 * compact spoken-length form over raw seconds so large downloads don't read as
 * "3600s remaining". Returns `—` for non-finite inputs.
 */
export function formatEta(seconds: number): string {
  if (!Number.isFinite(seconds) || seconds < 0) return "—";
  const s = Math.max(1, Math.round(seconds));
  if (s < 60) return `${s}s`;
  if (s < 3600) {
    const m = Math.floor(s / 60);
    const rem = s % 60;
    return rem === 0 ? `${m}m` : `${m}m ${rem}s`;
  }
  const h = Math.floor(s / 3600);
  const m = Math.floor((s % 3600) / 60);
  return m === 0 ? `${h}h` : `${h}h ${m}m`;
}

/**
 * Render the text line shown next to the progress bar. Handles three cases:
 *   - unknown total (`total_bytes === 0`): show downloaded-only
 *   - error status: show the translated error
 *   - otherwise: show downloaded / total + ETA
 *
 * ETA = (total - downloaded) * elapsed / downloaded. While `bytes_downloaded`
 * is 0 we can't divide yet, so we fall back to the downloaded-only string
 * rather than rendering `NaN`.
 */
export function describeDownloadProgress(
  progress: DownloadProgress,
  t: TFunction,
): string {
  if (progress.status === "error") {
    return t("settings.models.downloadError", {
      message: progress.model_name,
    });
  }
  const downloaded = formatDownloadedMB(progress.bytes_downloaded);
  if (progress.total_bytes === 0 || progress.bytes_downloaded === 0) {
    return t("settings.models.downloadProgressUnknown", { downloaded });
  }
  const remainingBytes = Math.max(
    0,
    progress.total_bytes - progress.bytes_downloaded,
  );
  const etaSeconds =
    (remainingBytes * (progress.elapsed_ms / 1000)) /
    progress.bytes_downloaded;
  return t("settings.models.downloadProgressKnown", {
    downloaded,
    total: formatDownloadedMB(progress.total_bytes),
    eta: formatEta(etaSeconds),
  });
}

/**
 * Map a model filename to an `settings.modelGuidance.*` i18n key, or null if
 * the model has no tier-based guidance. Keyed off filename (stable identifier)
 * rather than the display name so translated model names don't break lookup.
 */
function guidanceKeyForModel(filename: string): string | null {
  switch (filename) {
    case "ggml-tiny.en.bin":
      return "settings.modelGuidance.tinyEn";
    case "ggml-base.en.bin":
      return "settings.modelGuidance.baseEn";
    case "ggml-small.en.bin":
      return "settings.modelGuidance.smallEn";
    case "ggml-medium.en.bin":
      return "settings.modelGuidance.mediumEn";
    case "ggml-large-v3.bin":
      return "settings.modelGuidance.largeV3";
    case "lfm2-350m-extract-q4_k_m.gguf":
      return "settings.modelGuidance.lfm2_350m";
    default:
      return null;
  }
}

interface CredentialsManagerProps {
  state: Pick<SettingsState, "confirmDelete" | "logLevel">;
  t: TFunction;
  models: ModelInfo[];
  modelStatus: ModelStatus | null;
  isDownloading: boolean;
  isDeletingModel: string | null;
  downloadProgress: DownloadProgress | null;
  downloadModel: (filename: string) => void;
  handleDeleteClick: (filename: string) => void;
  handleLogLevelChange: (next: LogLevel) => Promise<void>;
}

/**
 * Managed stores shown to the user: downloaded model files (the primary
 * on-disk credential-like assets) and backend diagnostic log level. These
 * two regions live together here so SettingsPage stays a thin orchestrator.
 */
export default function CredentialsManager({
  state,
  t,
  models,
  modelStatus,
  isDownloading,
  isDeletingModel,
  downloadProgress,
  downloadModel,
  handleDeleteClick,
  handleLogLevelChange,
}: CredentialsManagerProps) {
  const { confirmDelete, logLevel } = state;

  return (
    <>
      <div id="settings-models-section" className="settings-section">
        <h3 className="settings-section__title">{t("settings.sections.models")}</h3>
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
          // Match on model_id (== filename) when available; fall back to
          // display name for compatibility with events that haven't been
          // re-emitted since the payload shape widened.
          const progressMatches = downloadProgress
            ? downloadProgress.model_id === model.filename ||
              downloadProgress.model_name === model.name
            : false;
          const isThisDownloading = isDownloading && progressMatches;
          const showProgressLine =
            progressMatches &&
            downloadProgress !== null &&
            downloadProgress.status !== "complete";
          const isThisDeleting = isDeletingModel === model.filename;

          return (
            <div className="model-card" key={model.filename}>
              <div className="model-card__header">
                <div>
                  <span className="model-card__name">{model.name}</span>
                  <span className={`status-badge ${badge.cls}`}>
                    {t(badge.labelKey)}
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
              {(() => {
                const gk = guidanceKeyForModel(model.filename);
                return gk ? (
                  <p
                    className="model-card__hint"
                    data-testid={`model-guidance-${model.filename}`}
                  >
                    {t(gk)}
                  </p>
                ) : null;
              })()}

              <div className="model-card__actions">
                {!model.is_downloaded && (
                  <button
                    className="settings-btn settings-btn--primary"
                    onClick={() => downloadModel(model.filename)}
                    disabled={isDownloading}
                  >
                    {isThisDownloading ? t("settings.buttons.downloading") : t("settings.buttons.download")}
                  </button>
                )}
                {model.is_downloaded && (
                  <button
                    className="settings-btn settings-btn--danger"
                    onClick={() => handleDeleteClick(model.filename)}
                    disabled={isThisDeleting}
                  >
                    {isThisDeleting
                      ? t("settings.buttons.deleting")
                      : confirmDelete === model.filename
                        ? t("settings.buttons.confirmDelete")
                        : t("settings.buttons.delete")}
                  </button>
                )}
              </div>

              {/* Download progress bar + ETA text */}
              {showProgressLine && downloadProgress && (
                <>
                  <div className="download-progress">
                    <div
                      className="download-progress__bar"
                      style={{ width: `${downloadProgress.percent}%` }}
                    />
                  </div>
                  <p
                    className="model-card__hint"
                    data-testid={`model-progress-${model.filename}`}
                  >
                    {describeDownloadProgress(downloadProgress, t)}
                  </p>
                </>
              )}
            </div>
          );
        })}
        {models.length === 0 && (
          <p className="settings-section__empty">{t("settings.models.empty")}</p>
        )}
      </div>

      <div className="settings-section">
        <h3 className="settings-section__title">
          {t("settings.sections.diagnostics")}
        </h3>
        <div className="settings-section__api-fields">
          <div className="settings-field">
            <label className="settings-field__label" htmlFor="log-level-select">
              {t("settings.fields.backendLogLevel")}
            </label>
            <select
              id="log-level-select"
              className="settings-input"
              value={logLevel}
              onChange={(e) =>
                handleLogLevelChange(e.target.value as LogLevel)
              }
            >
              <option value="off">{t("settings.logLevels.off")}</option>
              <option value="error">{t("settings.logLevels.error")}</option>
              <option value="warn">{t("settings.logLevels.warn")}</option>
              <option value="info">{t("settings.logLevels.info")}</option>
              <option value="debug">{t("settings.logLevels.debug")}</option>
              <option value="trace">{t("settings.logLevels.trace")}</option>
            </select>
            <p className="settings-hint">
              {t("settings.hints.logLevelPrefix")}{" "}
              <code>RUST_LOG</code>{" "}
              {t("settings.hints.logLevelSuffix")}
            </p>
          </div>
        </div>
      </div>
    </>
  );
}
