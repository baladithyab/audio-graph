/**
 * ASR provider sub-form — the choose-your-ASR surface inside
 * `SettingsPage`.
 *
 * Renders one of several backend-specific sub-panels based on the
 * reducer's current `asrType`:
 *   - `local_whisper`  — Whisper model file + language picker.
 *   - `api`            — OpenAI-compatible streaming endpoint + key.
 *   - `aws_transcribe` — region + credential-mode selector
 *                        (`default_chain` / `profile` / `access_keys`).
 *   - `deepgram`       — API key + Deepgram model pick (nova-3, etc.).
 *   - `assemblyai`     — API key.
 *   - `sherpa_onnx`    — streaming Zipformer model selector (behind the
 *                        `sherpa-streaming` cargo feature).
 *
 * Parent: `SettingsPage.tsx`. Props: a narrowed reducer slice + dispatch
 * + translation handle + `testingKey` so concurrent "Test connection"
 * buttons stay disabled while any test is in flight.
 */
import type { Dispatch, ReactNode } from "react";
import type { TFunction } from "i18next";
import { invoke } from "@tauri-apps/api/core";
import {
  readinessBadge,
  setField,
  type AwsCredentialMode,
  type SettingsAction,
  type SettingsState,
  type TestKey,
} from "./settingsTypes";
import type { ModelStatus } from "../types";

interface AsrProviderSettingsProps {
  state: Pick<
    SettingsState,
    | "asrType"
    | "whisperModel"
    | "asrEndpoint"
    | "asrApiKey"
    | "asrModel"
    | "awsAsrRegion"
    | "awsAsrLanguageCode"
    | "awsAsrCredentialMode"
    | "awsAsrProfileName"
    | "awsAsrAccessKey"
    | "awsAsrSecretKey"
    | "awsAsrSessionToken"
    | "awsAsrDiarization"
    | "deepgramApiKey"
    | "deepgramModel"
    | "deepgramDiarization"
    | "assemblyaiApiKey"
    | "assemblyaiDiarization"
    | "sherpaModelDir"
    | "sherpaEndpointDetection"
    | "awsProfiles"
    | "testingKey"
  >;
  dispatch: Dispatch<SettingsAction>;
  t: TFunction;
  modelStatus: ModelStatus | null;
  refreshAwsProfiles: () => Promise<void>;
  handleTestAsrApi: () => Promise<void>;
  handleTestDeepgram: () => Promise<void>;
  handleTestAssemblyAI: () => Promise<void>;
  handleTestAwsAsr: () => Promise<void>;
  handleClearCredential: (
    key: string,
    label: string,
    clearLocal: () => void,
  ) => Promise<void>;
  renderTestResult: (key: TestKey) => ReactNode;
}

export default function AsrProviderSettings({
  state,
  dispatch,
  t,
  modelStatus,
  refreshAwsProfiles,
  handleTestAsrApi,
  handleTestDeepgram,
  handleTestAssemblyAI,
  handleTestAwsAsr,
  handleClearCredential,
  renderTestResult,
}: AsrProviderSettingsProps) {
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
    awsProfiles,
    testingKey,
  } = state;

  return (
    <div className="settings-section">
      <h3 className="settings-section__title">{t("settings.sections.asr")}</h3>
      <div className="settings-radio-group">
        <label className="settings-radio">
          <input
            type="radio"
            name="asr-provider"
            checked={asrType === "local_whisper"}
            onChange={() => dispatch(setField("asrType", "local_whisper"))}
          />
          <span>{t("settings.asrProviders.localWhisper")}</span>
          {asrType === "local_whisper" && modelStatus && (
            <span
              className={`status-badge ${readinessBadge(modelStatus.whisper).cls}`}
            >
              {t(readinessBadge(modelStatus.whisper).labelKey)}
            </span>
          )}
        </label>

        {asrType === "local_whisper" && (
          <div className="settings-section__api-fields">
            <div className="settings-field">
              <label className="settings-field__label">{t("settings.fields.whisperModelSize")}</label>
              <select
                className="settings-input"
                value={whisperModel}
                onChange={(e) => dispatch(setField("whisperModel", e.target.value))}
              >
                <option value="ggml-tiny.en.bin">{t("settings.whisperModels.tiny")}</option>
                <option value="ggml-base.en.bin">{t("settings.whisperModels.base")}</option>
                <option value="ggml-small.en.bin">{t("settings.whisperModels.small")}</option>
                <option value="ggml-medium.en.bin">{t("settings.whisperModels.medium")}</option>
                <option value="ggml-large-v3.bin">{t("settings.whisperModels.large")}</option>
              </select>
            </div>
          </div>
        )}

        <label className="settings-radio">
          <input
            type="radio"
            name="asr-provider"
            checked={asrType === "api"}
            onChange={() => dispatch(setField("asrType", "api"))}
          />
          <span>{t("settings.asrProviders.cloudApi")}</span>
        </label>
        <label className="settings-radio">
          <input
            type="radio"
            name="asr-provider"
            checked={asrType === "aws_transcribe"}
            onChange={() => dispatch(setField("asrType", "aws_transcribe"))}
          />
          <span>{t("settings.asrProviders.awsTranscribe")}</span>
        </label>
        <label className="settings-radio">
          <input
            type="radio"
            name="asr-provider"
            checked={asrType === "deepgram"}
            onChange={() => dispatch(setField("asrType", "deepgram"))}
          />
          <span>{t("settings.asrProviders.deepgram")}</span>
        </label>
        <label className="settings-radio">
          <input
            type="radio"
            name="asr-provider"
            checked={asrType === "assemblyai"}
            onChange={() => dispatch(setField("asrType", "assemblyai"))}
          />
          <span>{t("settings.asrProviders.assemblyai")}</span>
        </label>
        <label className="settings-radio">
          <input
            type="radio"
            name="asr-provider"
            checked={asrType === "sherpa_onnx"}
            onChange={() => dispatch(setField("asrType", "sherpa_onnx"))}
          />
          <span>{t("settings.asrProviders.sherpaOnnx")}</span>
        </label>
      </div>

      {asrType === "api" && (
        <div className="settings-section__api-fields">
          <div className="settings-field">
            <label className="settings-field__label">
              {t("settings.fields.endpoint")}
            </label>
            <input
              className="settings-input"
              type="text"
              value={asrEndpoint}
              onChange={(e) => dispatch(setField("asrEndpoint", e.target.value))}
              placeholder="https://api.openai.com/v1"
            />
          </div>
          <div className="settings-field">
            <label className="settings-field__label">{t("settings.fields.apiKey")}</label>
            <input
              className="settings-input"
              type="password"
              value={asrApiKey}
              onChange={(e) => dispatch(setField("asrApiKey", e.target.value))}
              placeholder="sk-..."
            />
          </div>
          <div className="settings-field">
            <label className="settings-field__label">{t("settings.fields.model")}</label>
            <input
              className="settings-input"
              type="text"
              value={asrModel}
              onChange={(e) => dispatch(setField("asrModel", e.target.value))}
              placeholder="whisper-1"
            />
          </div>
          <div className="settings-field">
            <button
              type="button"
              className="settings-btn settings-btn--secondary"
              disabled={testingKey !== null || !asrEndpoint}
              onClick={handleTestAsrApi}
            >
              {testingKey === "asr_api" ? t("settings.buttons.testing") : t("settings.buttons.testConnection")}
            </button>
            {renderTestResult("asr_api")}
          </div>
        </div>
      )}

      {asrType === "aws_transcribe" && (
        <div className="settings-section__api-fields">
          <div className="settings-field">
            <label className="settings-field__label">{t("settings.fields.region")}</label>
            <input
              className="settings-input"
              type="text"
              value={awsAsrRegion}
              onChange={(e) => dispatch(setField("awsAsrRegion", e.target.value))}
              placeholder="us-east-1"
            />
          </div>
          <div className="settings-field">
            <label className="settings-field__label">
              {t("settings.fields.languageCode")}
            </label>
            <input
              className="settings-input"
              type="text"
              value={awsAsrLanguageCode}
              onChange={(e) => dispatch(setField("awsAsrLanguageCode", e.target.value))}
              placeholder="en-US"
            />
          </div>
          <div className="settings-field">
            <label className="settings-field__label">
              {t("settings.fields.credentialMode")}
            </label>
            <select
              className="settings-input"
              value={awsAsrCredentialMode}
              onChange={(e) =>
                dispatch(
                  setField(
                    "awsAsrCredentialMode",
                    e.target.value as AwsCredentialMode,
                  ),
                )
              }
            >
              <option value="default_chain">{t("settings.credentialModes.defaultChain")}</option>
              <option value="profile">{t("settings.credentialModes.profile")}</option>
              <option value="access_keys">{t("settings.credentialModes.accessKeys")}</option>
            </select>
          </div>
          {awsAsrCredentialMode === "profile" && (
            <div className="settings-field">
              <label className="settings-field__label">
                {t("settings.fields.awsProfile")}
              </label>
              <div className="settings-inline-row">
                <select
                  className="settings-input"
                  value={awsAsrProfileName}
                  onChange={(e) =>
                    dispatch(setField("awsAsrProfileName", e.target.value))
                  }
                >
                  <option value="">{t("settings.placeholders.selectProfile")}</option>
                  {awsProfiles.map((name) => (
                    <option key={name} value={name}>
                      {name}
                    </option>
                  ))}
                </select>
                <button
                  type="button"
                  className="settings-btn settings-btn--secondary"
                  onClick={refreshAwsProfiles}
                >
                  {t("settings.buttons.refresh")}
                </button>
              </div>
              {awsProfiles.length === 0 && (
                <p className="settings-hint">
                  {t("settings.hints.noAwsProfiles")}{" "}
                  <code>aws configure</code>{" "}
                  {t("settings.hints.noAwsProfilesSuffix")}
                </p>
              )}
            </div>
          )}
          {awsAsrCredentialMode === "access_keys" && (
            <>
              <div className="settings-field">
                <label className="settings-field__label">
                  {t("settings.fields.accessKeyId")}
                </label>
                <input
                  className="settings-input"
                  type="password"
                  value={awsAsrAccessKey}
                  onChange={(e) => dispatch(setField("awsAsrAccessKey", e.target.value))}
                  placeholder="AKIA..."
                />
              </div>
              <div className="settings-field">
                <label className="settings-field__label">
                  {t("settings.fields.secretAccessKey")}
                </label>
                <input
                  className="settings-input"
                  type="password"
                  value={awsAsrSecretKey}
                  onChange={(e) => dispatch(setField("awsAsrSecretKey", e.target.value))}
                  placeholder="wJalr..."
                />
              </div>
              <div className="settings-field">
                <label className="settings-field__label">
                  {t("settings.fields.sessionTokenOptional")}
                </label>
                <input
                  className="settings-input"
                  type="password"
                  value={awsAsrSessionToken}
                  onChange={(e) =>
                    dispatch(setField("awsAsrSessionToken", e.target.value))
                  }
                  placeholder={t("settings.placeholders.sessionTokenHint")}
                />
              </div>
              <div className="settings-field">
                <button
                  type="button"
                  className="settings-btn settings-btn--danger"
                  onClick={() =>
                    // AWS secret + token are shared between ASR and Bedrock
                    // forms, so clear both UI mirrors at once.
                    handleClearCredential(
                      "aws_secret_key",
                      t("settings.credentialConfirm.awsKeysLabel"),
                      () => {
                        dispatch({ type: "CLEAR_AWS_SHARED_KEYS" });
                        // Also drop the session token entry from the
                        // store; keep calls sequential so one failure
                        // doesn't leave a half-cleared state silently.
                        invoke("delete_credential_cmd", {
                          key: "aws_session_token",
                        }).catch((e) =>
                          console.error(
                            "Failed to clear aws_session_token:",
                            e,
                          ),
                        );
                      },
                    )
                  }
                >
                  {t("settings.buttons.clearSavedAwsKeys")}
                </button>
              </div>
            </>
          )}
          <div className="settings-field">
            <label className="settings-radio">
              <input
                type="checkbox"
                checked={awsAsrDiarization}
                onChange={(e) => dispatch(setField("awsAsrDiarization", e.target.checked))}
              />
              <span>{t("settings.fields.enableDiarization")}</span>
            </label>
          </div>
          <div className="settings-field">
            <button
              type="button"
              className="settings-btn settings-btn--secondary"
              disabled={testingKey !== null || !awsAsrRegion}
              onClick={handleTestAwsAsr}
            >
              {testingKey === "aws_asr" ? t("settings.buttons.testing") : t("settings.buttons.testConnection")}
            </button>
            {renderTestResult("aws_asr")}
          </div>
        </div>
      )}

      {asrType === "deepgram" && (
        <div className="settings-section__api-fields">
          <div className="settings-field">
            <label className="settings-field__label">{t("settings.fields.apiKey")}</label>
            <input
              className="settings-input"
              type="password"
              value={deepgramApiKey}
              onChange={(e) => dispatch(setField("deepgramApiKey", e.target.value))}
              placeholder="dg-..."
            />
          </div>
          <div className="settings-field">
            <label className="settings-field__label">{t("settings.fields.model")}</label>
            <input
              className="settings-input"
              type="text"
              value={deepgramModel}
              onChange={(e) => dispatch(setField("deepgramModel", e.target.value))}
              placeholder="nova-3"
            />
          </div>
          <div className="settings-field">
            <label className="settings-radio">
              <input
                type="checkbox"
                checked={deepgramDiarization}
                onChange={(e) =>
                  dispatch(setField("deepgramDiarization", e.target.checked))
                }
              />
              <span>{t("settings.fields.enableDiarization")}</span>
            </label>
          </div>
          <div className="settings-field">
            <button
              type="button"
              className="settings-btn settings-btn--secondary"
              disabled={testingKey !== null || !deepgramApiKey}
              onClick={handleTestDeepgram}
            >
              {testingKey === "deepgram" ? t("settings.buttons.testing") : t("settings.buttons.testConnection")}
            </button>
            {renderTestResult("deepgram")}
          </div>
        </div>
      )}

      {asrType === "assemblyai" && (
        <div className="settings-section__api-fields">
          <div className="settings-field">
            <label className="settings-field__label">{t("settings.fields.apiKey")}</label>
            <input
              className="settings-input"
              type="password"
              value={assemblyaiApiKey}
              onChange={(e) => dispatch(setField("assemblyaiApiKey", e.target.value))}
              placeholder={t("settings.placeholders.assemblyaiApiKey")}
            />
          </div>
          <div className="settings-field">
            <label className="settings-radio">
              <input
                type="checkbox"
                checked={assemblyaiDiarization}
                onChange={(e) =>
                  dispatch(setField("assemblyaiDiarization", e.target.checked))
                }
              />
              <span>{t("settings.fields.enableDiarization")}</span>
            </label>
          </div>
          <div className="settings-field">
            <button
              type="button"
              className="settings-btn settings-btn--secondary"
              disabled={testingKey !== null || !assemblyaiApiKey}
              onClick={handleTestAssemblyAI}
            >
              {testingKey === "assemblyai" ? t("settings.buttons.testing") : t("settings.buttons.testConnection")}
            </button>
            {renderTestResult("assemblyai")}
          </div>
        </div>
      )}

      {asrType === "sherpa_onnx" && (
        <div className="settings-section__api-fields">
          <div className="settings-field">
            <label className="settings-field__label">{t("settings.fields.modelDirectory")}</label>
            <input
              className="settings-input"
              type="text"
              value={sherpaModelDir}
              onChange={(e) => dispatch(setField("sherpaModelDir", e.target.value))}
              placeholder="streaming-zipformer-en-20M"
            />
          </div>
          <div className="settings-field">
            <label className="settings-radio">
              <input
                type="checkbox"
                checked={sherpaEndpointDetection}
                onChange={(e) => dispatch(setField("sherpaEndpointDetection", e.target.checked))}
              />
              <span>{t("settings.fields.enableEndpointDetection")}</span>
            </label>
          </div>
        </div>
      )}
    </div>
  );
}
