import { useState, useEffect, useCallback } from "react";
import { useAudioGraphStore } from "../store";

function ControlBar() {
  const isCapturing = useAudioGraphStore((s) => s.isCapturing);
  const isTranscribing = useAudioGraphStore((s) => s.isTranscribing);
  const isGeminiActive = useAudioGraphStore((s) => s.isGeminiActive);
  const selectedSourceIds = useAudioGraphStore((s) => s.selectedSourceIds);
  const audioSources = useAudioGraphStore((s) => s.audioSources);
  const captureStartTime = useAudioGraphStore((s) => s.captureStartTime);
  const settings = useAudioGraphStore((s) => s.settings);
  const startCapture = useAudioGraphStore((s) => s.startCapture);
  const stopCapture = useAudioGraphStore((s) => s.stopCapture);
  const startTranscribe = useAudioGraphStore((s) => s.startTranscribe);
  const stopTranscribe = useAudioGraphStore((s) => s.stopTranscribe);
  const startGemini = useAudioGraphStore((s) => s.startGemini);
  const stopGemini = useAudioGraphStore((s) => s.stopGemini);
  const openSettings = useAudioGraphStore((s) => s.openSettings);

  const [elapsed, setElapsed] = useState("00:00");

  // Update elapsed timer every second while capturing
  useEffect(() => {
    if (!isCapturing || captureStartTime === null) {
      setElapsed("00:00");
      return;
    }

    const tick = () => {
      const diff = Math.floor((Date.now() - captureStartTime) / 1000);
      const mins = Math.floor(diff / 60)
        .toString()
        .padStart(2, "0");
      const secs = (diff % 60).toString().padStart(2, "0");
      setElapsed(`${mins}:${secs}`);
    };

    tick(); // Immediate first tick
    const interval = setInterval(tick, 1000);
    return () => clearInterval(interval);
  }, [isCapturing, captureStartTime]);

  const handleToggleCapture = useCallback(async () => {
    if (isCapturing) {
      await stopCapture();
    } else {
      await startCapture();
    }
  }, [isCapturing, startCapture, stopCapture]);

  const handleToggleTranscribe = useCallback(async () => {
    if (isTranscribing) {
      await stopTranscribe();
    } else {
      await startTranscribe();
    }
  }, [isTranscribing, startTranscribe, stopTranscribe]);

  const handleToggleGemini = useCallback(async () => {
    if (isGeminiActive) {
      await stopGemini();
    } else {
      await startGemini();
    }
  }, [isGeminiActive, startGemini, stopGemini]);

  // Find selected source names
  const selectedSources = audioSources.filter((s) =>
    selectedSourceIds.includes(s.id),
  );
  const canStart = selectedSourceIds.length > 0 && !isCapturing;
  // Transcribe requires capture to be running
  const canTranscribe = isCapturing && !isTranscribing;
  // Gemini requires capture + a configured API key
  const hasGeminiKey = Boolean(
    settings?.gemini?.auth?.type === "api_key" && settings.gemini.auth.api_key
  ) || settings?.gemini?.auth?.type === "vertex_ai";
  const canGemini = isCapturing && !isGeminiActive && hasGeminiKey;
  const selectedLabel = selectedSources.map((s) => s.name).join(", ");

  // Both pipelines running simultaneously = comparison mode
  const isComparing = isTranscribing && isGeminiActive;

  return (
    <header
      className="control-bar"
      role="toolbar"
      aria-label="Capture controls"
    >
      <div className="control-bar__left">
        <h1 className="control-bar__title">AudioGraph</h1>
      </div>

      <div className="control-bar__center">
        {/* ── Capture controls ────────────────────────────────── */}
        <button
          className={`control-bar__capture-btn ${isCapturing ? "control-bar__capture-btn--stop" : "control-bar__capture-btn--start"}`}
          onClick={handleToggleCapture}
          disabled={!canStart && !isCapturing}
          aria-label={isCapturing ? "Stop capture" : "Start capture"}
        >
          {isCapturing ? "⏹ Stop" : "⏺ Start"}
        </button>

        {isCapturing && (
          <div className="control-bar__recording">
            <span className="control-bar__rec-dot" aria-hidden="true" />
            <span className="control-bar__timer">{elapsed}</span>
          </div>
        )}

        {/* ── Pipeline controls (visible when capturing) ──────── */}
        {isCapturing && (
          <>
            <span className="control-bar__separator" aria-hidden="true">|</span>
            <span className="control-bar__group-label">Pipelines</span>

            <button
              className={`control-bar__transcribe-btn ${isTranscribing ? "control-bar__transcribe-btn--active" : ""}`}
              onClick={handleToggleTranscribe}
              disabled={!canTranscribe && !isTranscribing}
              aria-label={isTranscribing ? "Stop transcription" : "Start transcription"}
              title="Stream audio to local Whisper ASR"
            >
              {isTranscribing && (
                <span className="control-bar__transcribe-dot" aria-hidden="true" />
              )}
              {isTranscribing ? "Stop Transcribe" : "Transcribe"}
            </button>

            <button
              className={`control-bar__gemini-btn ${isGeminiActive ? "control-bar__gemini-btn--active" : ""}`}
              onClick={handleToggleGemini}
              disabled={!canGemini && !isGeminiActive}
              aria-label={isGeminiActive ? "Stop Gemini" : "Start Gemini"}
              title={
                !hasGeminiKey
                  ? "Configure Gemini API key in Settings"
                  : "Stream audio to Gemini Live"
              }
            >
              {isGeminiActive && (
                <span className="control-bar__gemini-dot" aria-hidden="true" />
              )}
              {isGeminiActive ? "Stop Gemini" : "Gemini"}
            </button>

            {isComparing && (
              <span className="control-bar__comparing" title="Both local and Gemini pipelines are running">
                Comparing...
              </span>
            )}
          </>
        )}

        {/* ── Idle hints ─────────────────────────────────────── */}
        {!isCapturing && selectedSources.length > 0 && (
          <span className="control-bar__source-name" title={selectedLabel}>
            {selectedSources.length === 1
              ? selectedLabel
              : `${selectedSources.length} sources selected`}
          </span>
        )}

        {selectedSourceIds.length === 0 && !isCapturing && (
          <span className="control-bar__hint">
            Select audio sources to begin
          </span>
        )}
      </div>

      <div className="control-bar__right">
        {isCapturing && selectedSources.length > 0 && (
          <span className="control-bar__active-source">
            🎧{" "}
            {selectedSources.length === 1
              ? selectedLabel
              : `${selectedSources.length} sources`}
          </span>
        )}
        <button
          className="control-bar__settings-btn"
          onClick={openSettings}
          title="Settings"
        >
          ⚙️
        </button>
      </div>
    </header>
  );
}

export default ControlBar;
