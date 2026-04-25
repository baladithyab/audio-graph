/**
 * Speakers panel — compact list of detected speakers with per-speaker
 * colour swatch, human-readable talk time, and segment count badge.
 *
 * The backend diarization worker (see `src-tauri/src/diarization/mod.rs`)
 * emits `SPEAKER_DETECTED` events which `useTauriEvents` funnels into the
 * store. This component simply reflects that state — it is purely
 * presentational and carries no local UI state.
 *
 * Store bindings: `speakers`.
 *
 * Parent: `App.tsx` left panel. No props.
 */
import { useAudioGraphStore } from "../store";
import { formatDuration } from "../utils/format";

function SpeakerPanel() {
  const speakers = useAudioGraphStore((s) => s.speakers);

  return (
    <section className="panel speaker-panel" aria-label="Detected speakers">
      <div className="speaker-panel__header">
        <h3 className="panel-title">Speakers</h3>
        {speakers.length > 0 && (
          <span className="speaker-panel__count">{speakers.length}</span>
        )}
      </div>
      {speakers.length === 0 ? (
        <p className="panel-empty">No speakers detected yet</p>
      ) : (
        <ul className="speaker-list">
          {speakers.map((speaker) => (
            <li key={speaker.id} className="speaker-item">
              <span
                className="speaker-item__color"
                style={{ backgroundColor: speaker.color }}
                aria-hidden="true"
              />
              <div className="speaker-item__info">
                <span className="speaker-item__label">{speaker.label}</span>
                <span className="speaker-item__stats">
                  {formatDuration(speaker.total_speaking_time)} · {speaker.segment_count} segments
                </span>
              </div>
              <span className="speaker-item__badge">{speaker.segment_count}</span>
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}

export default SpeakerPanel;
