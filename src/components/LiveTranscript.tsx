import { useRef, useEffect, useMemo, useCallback, useState } from "react";
import { useAudioGraphStore } from "../store";
import { formatTime } from "../utils/format";
import {
  downloadAsFile,
  filenameTimestamp,
  transcriptToTxt,
} from "../utils/download";

/** Default fallback colors when speaker has no assigned color. */
const FALLBACK_COLORS = [
  "#60a5fa",
  "#f59e0b",
  "#10b981",
  "#ef4444",
  "#a78bfa",
  "#ec4899",
  "#6b7280",
];

function LiveTranscript() {
  const segments = useAudioGraphStore((s) => s.transcriptSegments);
  const speakers = useAudioGraphStore((s) => s.speakers);
  const exportTranscript = useAudioGraphStore((s) => s.exportTranscript);
  const getSessionId = useAudioGraphStore((s) => s.getSessionId);

  const scrollRef = useRef<HTMLDivElement>(null);
  const wasNearBottomRef = useRef(true);

  const [isExporting, setIsExporting] = useState(false);
  const [exportError, setExportError] = useState<string | null>(null);

  // Build a filename for an export in the form
  // `transcript-<sessionId>-<timestamp>.<ext>`. Falls back to "session" if
  // the backend session id can't be resolved.
  const buildFilename = useCallback(
    async (ext: "json" | "txt") => {
      let sessionId = "session";
      try {
        sessionId = await getSessionId();
      } catch {
        // Non-fatal — keep the fallback.
      }
      return `transcript-${sessionId}-${filenameTimestamp()}.${ext}`;
    },
    [getSessionId],
  );

  const handleExportJson = useCallback(async () => {
    setIsExporting(true);
    setExportError(null);
    try {
      const json = await exportTranscript();
      const filename = await buildFilename("json");
      downloadAsFile(json, filename, "application/json");
    } catch (e) {
      setExportError(e instanceof Error ? e.message : String(e));
    } finally {
      setIsExporting(false);
    }
  }, [exportTranscript, buildFilename]);

  const handleExportTxt = useCallback(async () => {
    setIsExporting(true);
    setExportError(null);
    try {
      const text = transcriptToTxt(segments);
      const filename = await buildFilename("txt");
      downloadAsFile(text, filename, "text/plain");
    } catch (e) {
      setExportError(e instanceof Error ? e.message : String(e));
    } finally {
      setIsExporting(false);
    }
  }, [segments, buildFilename]);

  // Build a quick speaker-color lookup
  const speakerColorMap = useMemo(() => {
    const map = new Map<string, string>();
    speakers.forEach((s) => {
      map.set(s.id, s.color);
    });
    return map;
  }, [speakers]);

  // Get color for a speaker, with fallback
  const getSpeakerColor = useCallback(
    (speakerId: string | null): string => {
      if (!speakerId) return FALLBACK_COLORS[0];
      const mapped = speakerColorMap.get(speakerId);
      if (mapped) return mapped;
      // Deterministic fallback based on id hash
      let hash = 0;
      for (let i = 0; i < speakerId.length; i++) {
        hash = (hash * 31 + speakerId.charCodeAt(i)) | 0;
      }
      return FALLBACK_COLORS[Math.abs(hash) % FALLBACK_COLORS.length];
    },
    [speakerColorMap]
  );

  // Auto-scroll: only if user is near the bottom
  useEffect(() => {
    const el = scrollRef.current;
    if (!el) return;

    // Check if we were near the bottom before the new segment arrived
    if (wasNearBottomRef.current) {
      el.scrollTop = el.scrollHeight;
    }
  }, [segments]);

  // Track scroll position to decide auto-scroll behavior
  const handleScroll = useCallback(() => {
    const el = scrollRef.current;
    if (!el) return;
    const distanceFromBottom =
      el.scrollHeight - el.scrollTop - el.clientHeight;
    wasNearBottomRef.current = distanceFromBottom < 100;
  }, []);

  // Display last 200 segments for performance
  const visibleSegments = useMemo(
    () => segments.slice(-200),
    [segments]
  );

  return (
    <div className="transcript">
      <div className="transcript__header">
        <h3 className="panel-title">Live Transcript</h3>
        <div className="transcript__header-actions">
          {segments.length > 0 && (
            <span className="transcript__count">{segments.length}</span>
          )}
          <button
            className="panel-export-btn"
            onClick={handleExportJson}
            disabled={isExporting || segments.length === 0}
            title="Export transcript as JSON"
            aria-label="Export transcript as JSON"
          >
            ⇩ JSON
          </button>
          <button
            className="panel-export-btn"
            onClick={handleExportTxt}
            disabled={isExporting || segments.length === 0}
            title="Export transcript as plain text"
            aria-label="Export transcript as plain text"
          >
            ⇩ TXT
          </button>
        </div>
      </div>
      {exportError && (
        <div className="panel-export-error" role="alert">
          Export failed: {exportError}
        </div>
      )}

      <div
        className="transcript__list"
        ref={scrollRef}
        onScroll={handleScroll}
        role="log"
        aria-live="polite"
        aria-label="Live transcript"
      >
        {visibleSegments.length === 0 ? (
          <div className="transcript__empty">
            <span className="transcript__empty-icon" aria-hidden="true">
              ═══
            </span>
            <p className="transcript__empty-text">Waiting for speech…</p>
          </div>
        ) : (
          visibleSegments.map((seg) => (
            <div key={seg.id} className="transcript__segment">
              <div className="transcript__segment-header">
                {seg.speaker_label && (
                  <span
                    className="transcript__speaker-badge"
                    style={{
                      backgroundColor: `${getSpeakerColor(seg.speaker_id)}20`,
                      color: getSpeakerColor(seg.speaker_id),
                      borderColor: `${getSpeakerColor(seg.speaker_id)}40`,
                    }}
                  >
                    {seg.speaker_label}
                  </span>
                )}
                <span className="transcript__timestamp">
                  {formatTime(seg.start_time)}
                </span>
              </div>
              <p className="transcript__text">{seg.text}</p>
              {seg.confidence < 1 && (
                <div
                  className="transcript__confidence"
                  role="meter"
                  aria-valuenow={Math.round(seg.confidence * 100)}
                  aria-valuemin={0}
                  aria-valuemax={100}
                  aria-label={`Confidence: ${Math.round(seg.confidence * 100)}%`}
                >
                  <div
                    className="transcript__confidence-fill"
                    style={{ width: `${seg.confidence * 100}%` }}
                  />
                </div>
              )}
            </div>
          ))
        )}
      </div>
    </div>
  );
}

export default LiveTranscript;
