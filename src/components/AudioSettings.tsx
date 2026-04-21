import type { Dispatch } from "react";
import type { TFunction } from "i18next";
import {
  setField,
  type ChannelCount,
  type SampleRate,
  type SettingsAction,
  type SettingsState,
} from "./settingsTypes";

interface AudioSettingsProps {
  state: Pick<SettingsState, "audioSampleRate" | "audioChannels">;
  dispatch: Dispatch<SettingsAction>;
  t: TFunction;
}

/**
 * Audio capture settings — sample rate + channel count. The downstream
 * pipeline always resamples to 16 kHz mono for ASR, so these controls only
 * change what the OS driver delivers (useful for matching a specific
 * interface's native rate, e.g. studio interfaces at 96 kHz).
 */
export default function AudioSettings({ state, dispatch, t }: AudioSettingsProps) {
  const { audioSampleRate, audioChannels } = state;
  return (
    <div className="settings-section">
      <h3 className="settings-section__title">
        {t("settings.sections.audio")}
      </h3>
      <div className="settings-section__api-fields">
        <div className="settings-field">
          <label
            className="settings-field__label"
            htmlFor="audio-sample-rate-select"
          >
            {t("settings.fields.captureSampleRate")}
          </label>
          <select
            id="audio-sample-rate-select"
            className="settings-input"
            value={audioSampleRate}
            onChange={(e) =>
              dispatch(
                setField(
                  "audioSampleRate",
                  Number(e.target.value) as SampleRate,
                ),
              )
            }
          >
            <option value={16000}>{t("settings.sampleRates.hz16000")}</option>
            <option value={22050}>{t("settings.sampleRates.hz22050")}</option>
            <option value={44100}>{t("settings.sampleRates.hz44100")}</option>
            <option value={48000}>{t("settings.sampleRates.hz48000")}</option>
            <option value={88200}>{t("settings.sampleRates.hz88200")}</option>
            <option value={96000}>{t("settings.sampleRates.hz96000")}</option>
          </select>
        </div>
        <div className="settings-field">
          <label
            className="settings-field__label"
            htmlFor="audio-channels-select"
          >
            {t("settings.fields.captureChannels")}
          </label>
          <select
            id="audio-channels-select"
            className="settings-input"
            value={audioChannels}
            onChange={(e) =>
              dispatch(
                setField(
                  "audioChannels",
                  Number(e.target.value) as ChannelCount,
                ),
              )
            }
          >
            <option value={1}>{t("settings.channels.mono")}</option>
            <option value={2}>{t("settings.channels.stereo")}</option>
          </select>
          <p className="settings-hint">
            {t("settings.hints.audioDownmix")}
          </p>
        </div>
      </div>
    </div>
  );
}
