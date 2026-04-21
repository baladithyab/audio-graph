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

interface LlmProviderSettingsProps {
  state: Pick<
    SettingsState,
    | "llmType"
    | "llmEndpoint"
    | "llmApiKey"
    | "llmModel"
    | "llmMaxTokens"
    | "llmTemperature"
    | "mistralrsModelId"
    | "awsBedrockRegion"
    | "awsBedrockModelId"
    | "awsBedrockCredentialMode"
    | "awsBedrockProfileName"
    | "awsBedrockAccessKey"
    | "awsBedrockSecretKey"
    | "awsBedrockSessionToken"
    | "awsProfiles"
    | "testingKey"
  >;
  dispatch: Dispatch<SettingsAction>;
  t: TFunction;
  modelStatus: ModelStatus | null;
  refreshAwsProfiles: () => Promise<void>;
  handleTestAwsBedrock: () => Promise<void>;
  handleClearCredential: (
    key: string,
    label: string,
    clearLocal: () => void,
  ) => Promise<void>;
  renderTestResult: (key: TestKey) => ReactNode;
}

export default function LlmProviderSettings({
  state,
  dispatch,
  t,
  modelStatus,
  refreshAwsProfiles,
  handleTestAwsBedrock,
  handleClearCredential,
  renderTestResult,
}: LlmProviderSettingsProps) {
  const {
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
    awsProfiles,
    testingKey,
  } = state;

  return (
    <div className="settings-section">
      <h3 className="settings-section__title">{t("settings.sections.llm")}</h3>
      <div className="settings-radio-group">
        <label className="settings-radio">
          <input
            type="radio"
            name="llm-provider"
            checked={llmType === "local_llama"}
            onChange={() => dispatch(setField("llmType", "local_llama"))}
          />
          <span>{t("settings.llmProviders.localLlama")}</span>
          {llmType === "local_llama" && modelStatus && (
            <span
              className={`status-badge ${readinessBadge(modelStatus.llm).cls}`}
            >
              {t(readinessBadge(modelStatus.llm).labelKey)}
            </span>
          )}
        </label>
        <label className="settings-radio">
          <input
            type="radio"
            name="llm-provider"
            checked={llmType === "api"}
            onChange={() => dispatch(setField("llmType", "api"))}
          />
          <span>{t("settings.llmProviders.openaiCompatible")}</span>
        </label>
        <label className="settings-radio">
          <input
            type="radio"
            name="llm-provider"
            checked={llmType === "aws_bedrock"}
            onChange={() => dispatch(setField("llmType", "aws_bedrock"))}
          />
          <span>{t("settings.llmProviders.awsBedrock")}</span>
        </label>
        <label className="settings-radio">
          <input
            type="radio"
            name="llm-provider"
            checked={llmType === "mistralrs"}
            onChange={() => dispatch(setField("llmType", "mistralrs"))}
          />
          <span>{t("settings.llmProviders.mistralrs")}</span>
        </label>
      </div>

      {llmType === "api" && (
        <div className="settings-section__api-fields">
          <div className="settings-field">
            <label className="settings-field__label">
              {t("settings.fields.endpoint")}
            </label>
            <input
              className="settings-input"
              type="text"
              value={llmEndpoint}
              onChange={(e) => dispatch(setField("llmEndpoint", e.target.value))}
              placeholder="https://openrouter.ai/api/v1"
            />
          </div>
          <div className="settings-field">
            <label className="settings-field__label">{t("settings.fields.apiKey")}</label>
            <input
              className="settings-input"
              type="password"
              value={llmApiKey}
              onChange={(e) => dispatch(setField("llmApiKey", e.target.value))}
              placeholder="sk-..."
            />
          </div>
          <div className="settings-field">
            <label className="settings-field__label">{t("settings.fields.model")}</label>
            <input
              className="settings-input"
              type="text"
              value={llmModel}
              onChange={(e) => dispatch(setField("llmModel", e.target.value))}
              placeholder="gpt-4o-mini"
            />
          </div>
          <div className="settings-field">
            <label className="settings-field__label">
              {t("settings.fields.maxTokens", { count: llmMaxTokens })}
            </label>
            <input
              className="settings-input"
              type="number"
              value={llmMaxTokens}
              onChange={(e) => dispatch(setField("llmMaxTokens", Number(e.target.value)))}
              min={1}
              max={32768}
            />
          </div>
          <div className="settings-field">
            <label className="settings-field__label">
              {t("settings.fields.temperature", { value: llmTemperature })}
            </label>
            <input
              className="settings-input"
              type="number"
              step="0.1"
              value={llmTemperature}
              onChange={(e) =>
                dispatch(setField("llmTemperature", Number(e.target.value)))
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
            <label className="settings-field__label">{t("settings.fields.region")}</label>
            <input
              className="settings-input"
              type="text"
              value={awsBedrockRegion}
              onChange={(e) => dispatch(setField("awsBedrockRegion", e.target.value))}
              placeholder="us-east-1"
            />
          </div>
          <div className="settings-field">
            <label className="settings-field__label">{t("settings.fields.modelId")}</label>
            <input
              className="settings-input"
              type="text"
              value={awsBedrockModelId}
              onChange={(e) => dispatch(setField("awsBedrockModelId", e.target.value))}
              placeholder="anthropic.claude-3-haiku-20240307-v1:0"
            />
          </div>
          <div className="settings-field">
            <label className="settings-field__label">
              {t("settings.fields.credentialMode")}
            </label>
            <select
              className="settings-input"
              value={awsBedrockCredentialMode}
              onChange={(e) =>
                dispatch(
                  setField(
                    "awsBedrockCredentialMode",
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
          {awsBedrockCredentialMode === "profile" && (
            <div className="settings-field">
              <label className="settings-field__label">
                {t("settings.fields.awsProfile")}
              </label>
              <div className="settings-inline-row">
                <select
                  className="settings-input"
                  value={awsBedrockProfileName}
                  onChange={(e) =>
                    dispatch(setField("awsBedrockProfileName", e.target.value))
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
          {awsBedrockCredentialMode === "access_keys" && (
            <>
              <div className="settings-field">
                <label className="settings-field__label">
                  {t("settings.fields.accessKeyId")}
                </label>
                <input
                  className="settings-input"
                  type="password"
                  value={awsBedrockAccessKey}
                  onChange={(e) =>
                    dispatch(setField("awsBedrockAccessKey", e.target.value))
                  }
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
                  value={awsBedrockSecretKey}
                  onChange={(e) =>
                    dispatch(setField("awsBedrockSecretKey", e.target.value))
                  }
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
                  value={awsBedrockSessionToken}
                  onChange={(e) =>
                    dispatch(setField("awsBedrockSessionToken", e.target.value))
                  }
                  placeholder={t("settings.placeholders.sessionTokenHint")}
                />
              </div>
              <div className="settings-field">
                <button
                  type="button"
                  className="settings-btn settings-btn--danger"
                  onClick={() =>
                    handleClearCredential(
                      "aws_secret_key",
                      t("settings.credentialConfirm.awsKeysLabel"),
                      () => {
                        dispatch({ type: "CLEAR_AWS_SHARED_KEYS" });
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
            <button
              type="button"
              className="settings-btn settings-btn--secondary"
              disabled={testingKey !== null || !awsBedrockRegion}
              onClick={handleTestAwsBedrock}
            >
              {testingKey === "aws_bedrock"
                ? t("settings.buttons.testing")
                : t("settings.buttons.testConnection")}
            </button>
            {renderTestResult("aws_bedrock")}
          </div>
        </div>
      )}

      {llmType === "mistralrs" && (
        <div className="settings-section__api-fields">
          <div className="settings-field">
            <label className="settings-field__label">{t("settings.fields.modelId")}</label>
            <input
              className="settings-input"
              type="text"
              value={mistralrsModelId}
              onChange={(e) => dispatch(setField("mistralrsModelId", e.target.value))}
              placeholder="ggml-small-extract.gguf"
            />
          </div>
        </div>
      )}
    </div>
  );
}
