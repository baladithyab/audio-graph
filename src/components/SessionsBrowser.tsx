/**
 * Sessions browser modal — lets the user inspect, restore, and delete
 * past capture sessions.
 *
 * Source of truth is the `sessions.json` index in `~/.audiograph/` —
 * `list_sessions` returns all known sessions, `load_session_transcript`
 * fetches a session's transcript for preview, `restore_session` makes
 * one the active session, `delete_session` soft-deletes (marks for
 * expiry), `delete_session_permanently` hard-deletes, and
 * `purge_expired_sessions` cleans up old soft-deletes.
 *
 * Sort mode (`newest | oldest | nameAsc | nameDesc | largest`) is
 * persisted to `localStorage` under `audiograph:sessionsBrowser:sort`
 * so it survives reloads independent of the Rust-side settings file.
 *
 * Focus-trapped via `useFocusTrap`. Escape handled at the app level by
 * `useKeyboardShortcuts`.
 *
 * Store bindings: `sessionsBrowserOpen`, `closeSessionsBrowser`.
 *
 * Parent: `App.tsx` (rendered conditionally). No props.
 */
import { useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { useAudioGraphStore } from "../store";
import { useFocusTrap } from "../hooks/useFocusTrap";
import type { SessionMetadata } from "../types";

/** Sort modes. Values double as i18n keys under `sessions.sort.*`. */
export type SessionSortMode =
    | "newest"
    | "oldest"
    | "nameAsc"
    | "nameDesc"
    | "largest";

const SORT_MODES: SessionSortMode[] = [
    "newest",
    "oldest",
    "nameAsc",
    "nameDesc",
    "largest",
];

/** localStorage key for the sort preference. */
const SORT_STORAGE_KEY = "audiograph:sessionsBrowser:sort";

function loadSortPreference(): SessionSortMode {
    try {
        const raw = localStorage.getItem(SORT_STORAGE_KEY);
        if (raw && (SORT_MODES as string[]).includes(raw)) {
            return raw as SessionSortMode;
        }
    } catch {
        // localStorage unavailable (SSR, permission-denied, etc.) — fall back.
    }
    return "newest";
}

function saveSortPreference(mode: SessionSortMode): void {
    try {
        localStorage.setItem(SORT_STORAGE_KEY, mode);
    } catch {
        // Non-fatal — preference just won't persist across restarts.
    }
}

/** Format a unix-millis timestamp into a short, human-readable local string. */
function formatTimestamp(ms: number): string {
    if (!ms) return "—";
    return new Date(ms).toLocaleString();
}

/** Format a duration in seconds as "Hh Mm" or "Mm Ss". */
function formatDuration(seconds: number | null): string {
    if (seconds === null || seconds === undefined) return "—";
    if (seconds < 60) return `${seconds}s`;
    const h = Math.floor(seconds / 3600);
    const m = Math.floor((seconds % 3600) / 60);
    const s = seconds % 60;
    if (h > 0) return `${h}h ${m}m`;
    return `${m}m ${s}s`;
}

/** CSS-class-friendly modifier for a session's status. */
function statusModifier(status: SessionMetadata["status"]): string {
    return `sessions-browser__status--${status}`;
}

/** Display name for a session — falls back to the short id. */
function displayName(s: SessionMetadata): string {
    return s.title ?? s.id.slice(0, 8);
}

/** Filter+sort pipeline. Exported for unit tests. */
export function applyFilterAndSort(
    sessions: SessionMetadata[],
    search: string,
    sortMode: SessionSortMode,
    showTrash: boolean,
): SessionMetadata[] {
    const needle = search.trim().toLowerCase();
    const filtered = sessions.filter((s) => {
        const isTrash = s.deleted === true;
        if (showTrash !== isTrash) return false;
        if (!needle) return true;
        const name = displayName(s).toLowerCase();
        return name.includes(needle) || s.id.toLowerCase().includes(needle);
    });

    const sorted = [...filtered];
    switch (sortMode) {
        case "newest":
            sorted.sort((a, b) => b.created_at - a.created_at);
            break;
        case "oldest":
            sorted.sort((a, b) => a.created_at - b.created_at);
            break;
        case "nameAsc":
            sorted.sort((a, b) =>
                displayName(a).localeCompare(displayName(b), undefined, {
                    sensitivity: "base",
                }),
            );
            break;
        case "nameDesc":
            sorted.sort((a, b) =>
                displayName(b).localeCompare(displayName(a), undefined, {
                    sensitivity: "base",
                }),
            );
            break;
        case "largest":
            sorted.sort((a, b) => b.segment_count - a.segment_count);
            break;
    }
    return sorted;
}

function SessionsBrowser() {
    const { t } = useTranslation();
    const modalRef = useFocusTrap<HTMLDivElement>();
    const sessions = useAudioGraphStore((s) => s.sessions);
    const sessionsLoading = useAudioGraphStore((s) => s.sessionsLoading);
    const listSessions = useAudioGraphStore((s) => s.listSessions);
    const loadSessionTranscript = useAudioGraphStore(
        (s) => s.loadSessionTranscript,
    );
    const deleteSession = useAudioGraphStore((s) => s.deleteSession);
    const restoreSession = useAudioGraphStore((s) => s.restoreSession);
    const deleteSessionPermanently = useAudioGraphStore(
        (s) => s.deleteSessionPermanently,
    );
    const closeSessionsBrowser = useAudioGraphStore(
        (s) => s.closeSessionsBrowser,
    );
    const setRightPanelTab = useAudioGraphStore((s) => s.setRightPanelTab);

    const [search, setSearch] = useState("");
    const [sortMode, setSortMode] = useState<SessionSortMode>(() =>
        loadSortPreference(),
    );
    const [showTrash, setShowTrash] = useState(false);

    // Refresh on mount — match the v2 store's own larger fetch (200) so the
    // browser's search can actually find old entries, not just the 10 most
    // recent the v1 overlay loaded.
    useEffect(() => {
        void listSessions(200);
    }, [listSessions]);

    const trashCount = useMemo(
        () => sessions.filter((s) => s.deleted === true).length,
        [sessions],
    );

    const visible = useMemo(
        () => applyFilterAndSort(sessions, search, sortMode, showTrash),
        [sessions, search, sortMode, showTrash],
    );

    const handleSortChange = (mode: SessionSortMode) => {
        setSortMode(mode);
        saveSortPreference(mode);
    };

    const handleLoad = async (sessionId: string) => {
        await loadSessionTranscript(sessionId);
        setRightPanelTab("transcript");
        closeSessionsBrowser();
    };

    const handleDelete = async (sessionId: string) => {
        const ok = window.confirm(t("sessions.deleteConfirm"));
        if (!ok) return;
        await deleteSession(sessionId);
    };

    const handleRestore = async (sessionId: string) => {
        await restoreSession(sessionId);
    };

    const handleDeletePermanently = async (sessionId: string) => {
        const ok = window.confirm(t("sessions.deletePermanentlyConfirm"));
        if (!ok) return;
        await deleteSessionPermanently(sessionId);
    };

    return (
        <div className="settings-overlay" onClick={closeSessionsBrowser}>
            <div
                ref={modalRef}
                className="settings-modal sessions-browser"
                onClick={(e) => e.stopPropagation()}
                role="dialog"
                aria-modal="true"
                aria-labelledby="sessions-browser-title"
                tabIndex={-1}
            >
                <div className="settings-header">
                    <h2
                        id="sessions-browser-title"
                        className="settings-header__title"
                    >
                        {t("sessions.title")}
                    </h2>
                    <button
                        className="settings-header__close"
                        onClick={closeSessionsBrowser}
                        aria-label={t("sessions.close")}
                    >
                        ✕
                    </button>
                </div>

                <div className="settings-content">
                    <div
                        className="sessions-browser__toolbar"
                        style={{
                            display: "flex",
                            flexWrap: "wrap",
                            gap: "8px",
                            alignItems: "center",
                            marginBottom: "12px",
                        }}
                    >
                        <input
                            type="search"
                            className="sessions-browser__search"
                            aria-label={t("sessions.searchLabel")}
                            placeholder={t("sessions.searchPlaceholder")}
                            value={search}
                            onChange={(e) => setSearch(e.target.value)}
                            style={{
                                flex: "1 1 200px",
                                minWidth: 0,
                                padding: "6px 10px",
                                borderRadius: "6px",
                                border: "1px solid var(--border, #333)",
                                background: "transparent",
                                color: "inherit",
                            }}
                        />
                        <label
                            style={{
                                display: "flex",
                                alignItems: "center",
                                gap: "6px",
                                fontSize: "0.85em",
                            }}
                        >
                            <span>{t("sessions.sortLabel")}</span>
                            <select
                                aria-label={t("sessions.sortLabel")}
                                value={sortMode}
                                onChange={(e) =>
                                    handleSortChange(
                                        e.target.value as SessionSortMode,
                                    )
                                }
                                style={{
                                    padding: "5px 8px",
                                    borderRadius: "6px",
                                    border: "1px solid var(--border, #333)",
                                    background: "transparent",
                                    color: "inherit",
                                }}
                            >
                                {SORT_MODES.map((m) => (
                                    <option key={m} value={m}>
                                        {t(`sessions.sort.${m}`)}
                                    </option>
                                ))}
                            </select>
                        </label>
                        <button
                            type="button"
                            className="settings-btn"
                            aria-pressed={showTrash}
                            onClick={() => setShowTrash((v) => !v)}
                            title={
                                showTrash
                                    ? t("sessions.hideTrash")
                                    : t("sessions.showTrash")
                            }
                        >
                            {showTrash
                                ? t("sessions.hideTrash")
                                : t("sessions.trashCount", { count: trashCount })}
                        </button>
                    </div>

                    {sessionsLoading ? (
                        <p>{t("common.loading")}</p>
                    ) : sessions.length === 0 ? (
                        <p className="settings-section__empty">
                            {t("sessions.noSessions")}
                        </p>
                    ) : visible.length === 0 ? (
                        <p className="settings-section__empty">
                            {t("sessions.noMatches")}
                        </p>
                    ) : (
                        <ul
                            className="sessions-browser__list"
                            style={{
                                listStyle: "none",
                                padding: 0,
                                margin: 0,
                                display: "flex",
                                flexDirection: "column",
                                gap: "8px",
                            }}
                        >
                            {visible.map((s) => (
                                <li
                                    key={s.id}
                                    className="sessions-browser__item"
                                    data-testid={`session-${s.id}`}
                                    data-trashed={s.deleted ? "true" : "false"}
                                    style={{
                                        border: "1px solid var(--border, #333)",
                                        borderRadius: "6px",
                                        padding: "10px 12px",
                                        display: "flex",
                                        flexDirection: "column",
                                        gap: "6px",
                                        opacity: s.deleted ? 0.7 : 1,
                                    }}
                                >
                                    <div
                                        style={{
                                            display: "flex",
                                            justifyContent: "space-between",
                                            alignItems: "baseline",
                                            gap: "8px",
                                        }}
                                    >
                                        <div
                                            style={{
                                                display: "flex",
                                                flexDirection: "column",
                                                gap: "2px",
                                                minWidth: 0,
                                            }}
                                        >
                                            <strong
                                                style={{
                                                    fontSize: "0.95em",
                                                    overflow: "hidden",
                                                    textOverflow: "ellipsis",
                                                    whiteSpace: "nowrap",
                                                }}
                                                title={s.id}
                                            >
                                                {displayName(s)}
                                            </strong>
                                            <span
                                                style={{
                                                    fontSize: "0.8em",
                                                    opacity: 0.7,
                                                }}
                                            >
                                                {s.deleted && s.deleted_at
                                                    ? t("sessions.trashedOn", {
                                                          date: formatTimestamp(
                                                              s.deleted_at,
                                                          ),
                                                      })
                                                    : formatTimestamp(
                                                          s.created_at,
                                                      )}
                                            </span>
                                        </div>
                                        <span
                                            className={`sessions-browser__status ${statusModifier(s.status)}`}
                                            style={{
                                                fontSize: "0.75em",
                                                padding: "2px 8px",
                                                borderRadius: "999px",
                                                border: "1px solid currentColor",
                                                opacity: 0.8,
                                                textTransform: "capitalize",
                                                whiteSpace: "nowrap",
                                            }}
                                        >
                                            {s.status}
                                        </span>
                                    </div>

                                    <div
                                        style={{
                                            fontSize: "0.8em",
                                            opacity: 0.75,
                                            display: "flex",
                                            gap: "12px",
                                            flexWrap: "wrap",
                                        }}
                                    >
                                        <span>
                                            {t("sessions.stats.duration")}:{" "}
                                            {formatDuration(s.duration_seconds)}
                                        </span>
                                        <span>
                                            {t("sessions.stats.segments")}:{" "}
                                            {s.segment_count}
                                        </span>
                                        <span>
                                            {t("sessions.stats.speakers")}:{" "}
                                            {s.speaker_count}
                                        </span>
                                        <span>
                                            {t("sessions.stats.entities")}:{" "}
                                            {s.entity_count}
                                        </span>
                                    </div>

                                    <div
                                        style={{
                                            display: "flex",
                                            gap: "8px",
                                            justifyContent: "flex-end",
                                            flexWrap: "wrap",
                                        }}
                                    >
                                        {s.deleted ? (
                                            <>
                                                <button
                                                    className="settings-btn"
                                                    onClick={() =>
                                                        handleRestore(s.id)
                                                    }
                                                >
                                                    {t("sessions.restore")}
                                                </button>
                                                <button
                                                    className="settings-btn settings-btn--danger"
                                                    onClick={() =>
                                                        handleDeletePermanently(
                                                            s.id,
                                                        )
                                                    }
                                                >
                                                    {t(
                                                        "sessions.deletePermanently",
                                                    )}
                                                </button>
                                            </>
                                        ) : (
                                            <>
                                                <button
                                                    className="settings-btn settings-btn--primary"
                                                    onClick={() =>
                                                        handleLoad(s.id)
                                                    }
                                                >
                                                    {t("sessions.load")}
                                                </button>
                                                <button
                                                    className="settings-btn settings-btn--danger"
                                                    onClick={() =>
                                                        handleDelete(s.id)
                                                    }
                                                >
                                                    {t("sessions.delete")}
                                                </button>
                                            </>
                                        )}
                                    </div>
                                </li>
                            ))}
                        </ul>
                    )}
                </div>
            </div>
        </div>
    );
}

export default SessionsBrowser;
