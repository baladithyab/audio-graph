/**
 * Keyboard shortcuts help modal — user-facing reference for the global
 * hotkeys registered by `useKeyboardShortcuts`.
 *
 * Kept in sync manually with the hook (the `SHORTCUTS` list here is
 * documentation, not a source of truth). Opened via Cmd/Ctrl+/ or "?"
 * and dismissed via Escape, the close button, or a backdrop click.
 *
 * Props:
 *   - `onClose`: invoked on dismiss; parent (`App.tsx`) clears its
 *     local `shortcutsOpen` state.
 *
 * Focus-trapped via `useFocusTrap`.
 */
import { useEffect } from "react";
import { useTranslation } from "react-i18next";
import { useFocusTrap } from "../hooks/useFocusTrap";

interface ShortcutsHelpModalProps {
    onClose: () => void;
}

type ShortcutEntry = {
    id: string;
    keys: string[];
};

// Mirrors the bindings declared in useKeyboardShortcuts.ts. Keep in sync
// manually — this list is user-facing documentation, not the source of truth.
const SHORTCUTS: readonly ShortcutEntry[] = [
    { id: "toggleCapture", keys: ["Cmd/Ctrl", "R"] },
    { id: "openSettings", keys: ["Cmd/Ctrl", ","] },
    { id: "openSessions", keys: ["Cmd/Ctrl", "Shift", "S"] },
    { id: "openHelp", keys: ["Cmd/Ctrl", "/"] },
    { id: "closeModal", keys: ["Esc"] },
];

function ShortcutsHelpModal({ onClose }: ShortcutsHelpModalProps) {
    const { t } = useTranslation();
    const modalRef = useFocusTrap<HTMLDivElement>();

    // Local Escape handler: the global useKeyboardShortcuts hook only closes
    // Settings/SessionsBrowser on Escape. We don't want to add this modal to
    // that hook (the task forbids touching it), so handle Escape here.
    useEffect(() => {
        const handler = (e: KeyboardEvent) => {
            if (e.key === "Escape") {
                e.preventDefault();
                onClose();
            }
        };
        window.addEventListener("keydown", handler);
        return () => window.removeEventListener("keydown", handler);
    }, [onClose]);

    return (
        <div className="settings-overlay" onClick={onClose}>
            <div
                ref={modalRef}
                className="settings-modal shortcuts-modal"
                onClick={(e) => e.stopPropagation()}
                role="dialog"
                aria-modal="true"
                aria-labelledby="shortcuts-modal-title"
                tabIndex={-1}
            >
                <div className="settings-header">
                    <h2
                        id="shortcuts-modal-title"
                        className="settings-header__title"
                    >
                        {t("shortcuts.title")}
                    </h2>
                    <button
                        className="settings-header__close"
                        onClick={onClose}
                        aria-label={t("shortcuts.close")}
                    >
                        ✕
                    </button>
                </div>

                <div className="settings-content">
                    <ul className="shortcuts-list">
                        {SHORTCUTS.map((s) => (
                            <li key={s.id} className="shortcuts-list__item">
                                <span className="shortcuts-list__keys">
                                    {s.keys.map((k, i) => (
                                        <span key={i} className="shortcuts-list__key-group">
                                            <kbd className="shortcuts-list__kbd">{k}</kbd>
                                            {i < s.keys.length - 1 && (
                                                <span className="shortcuts-list__plus" aria-hidden="true">
                                                    +
                                                </span>
                                            )}
                                        </span>
                                    ))}
                                </span>
                                <span className="shortcuts-list__desc">
                                    {t(`shortcuts.items.${s.id}`)}
                                </span>
                            </li>
                        ))}
                    </ul>
                </div>
            </div>
        </div>
    );
}

export default ShortcutsHelpModal;
