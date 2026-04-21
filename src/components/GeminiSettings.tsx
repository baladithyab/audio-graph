import type { Dispatch, ReactNode } from "react";
import type { TFunction } from "i18next";
import {
  setField,
  type SettingsAction,
  type SettingsState,
  type TestKey,
} from "./settingsTypes";

interface GeminiSettingsProps {
  state: Pick<
    SettingsState,
    | "geminiAuthMode"
    | "geminiApiKey"
    | "geminiModel"
    | "geminiProjectId"
    | "geminiLocation"
    | "geminiServiceAccountPath"
    | "testingKey"
  >;
  dispatch: Dispatch<SettingsAction>;
  t: TFunction;
  handleTestGemini: () => Promise<void>;
  renderTestResult: (key: TestKey) => ReactNode;
}

export default function GeminiSettings({
  state,
  dispatch,
  t,
  handleTestGemini,
  renderTestResult,
}: GeminiSettingsProps) {
  const {
    geminiAuthMode,
    geminiApiKey,
    geminiModel,
    geminiProjectId,
    geminiLocation,
    geminiServiceAccountPath,
    testingKey,
  } = state;

  return (
    <div className="settings-section">
      <h3 className="settings-section__title">{t("settings.sections.gemini")}</h3>
      <div className="settings-radio-group">
        <label className="settings-radio">
          <input
            type="radio"
            name="gemini-auth"
            checked={geminiAuthMode === "api_key"}
            onChange={() => dispatch(setField("geminiAuthMode", "api_key"))}
          />
          <span>{t("settings.geminiAuth.apiKey")}</span>
        </label>
        <label className="settings-radio">
          <input
            type="radio"
            name="gemini-auth"
            checked={geminiAuthMode === "vertex_ai"}
            onChange={() => dispatch(setField("geminiAuthMode", "vertex_ai"))}
          />
          <span>{t("settings.geminiAuth.vertexAi")}</span>
        </label>
      </div>

      <div className="settings-section__api-fields">
        {geminiAuthMode === "api_key" && (
          <>
            <div className="settings-field">
              <label className="settings-field__label">
                {t("settings.fields.geminiApiKey")}
              </label>
              <input
                className="settings-input"
                type="password"
                value={geminiApiKey}
                onChange={(e) => dispatch(setField("geminiApiKey", e.target.value))}
                placeholder="AIza..."
              />
            </div>
            <div className="settings-field">
              <button
                type="button"
                className="settings-btn settings-btn--secondary"
                disabled={testingKey !== null || !geminiApiKey}
                onClick={handleTestGemini}
              >
                {testingKey === "gemini"
                  ? t("settings.buttons.testing")
                  : t("settings.buttons.testConnection")}
              </button>
              {renderTestResult("gemini")}
            </div>
          </>
        )}

        {geminiAuthMode === "vertex_ai" && (
          <>
            <div className="settings-field">
              <label className="settings-field__label">
                {t("settings.fields.projectId")}
              </label>
              <input
                className="settings-input"
                type="text"
                value={geminiProjectId}
                onChange={(e) => dispatch(setField("geminiProjectId", e.target.value))}
                placeholder="my-gcp-project"
              />
            </div>
            <div className="settings-field">
              <label className="settings-field__label">{t("settings.fields.location")}</label>
              <input
                className="settings-input"
                type="text"
                value={geminiLocation}
                onChange={(e) => dispatch(setField("geminiLocation", e.target.value))}
                placeholder="us-central1"
              />
            </div>
            <div className="settings-field">
              <label className="settings-field__label">
                {t("settings.fields.serviceAccountPathOptional")}
              </label>
              <input
                className="settings-input"
                type="text"
                value={geminiServiceAccountPath}
                onChange={(e) =>
                  dispatch(setField("geminiServiceAccountPath", e.target.value))
                }
                placeholder="/path/to/service-account.json"
              />
            </div>
          </>
        )}

        <div className="settings-field">
          <label className="settings-field__label">{t("settings.fields.model")}</label>
          <input
            className="settings-input"
            type="text"
            value={geminiModel}
            onChange={(e) => dispatch(setField("geminiModel", e.target.value))}
            placeholder="gemini-3.1-flash-live-preview"
          />
        </div>
      </div>
    </div>
  );
}
