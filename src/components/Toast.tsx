import { useEffect, useState } from "react";

export type ToastVariant = "success" | "info";

export interface ToastPayload {
    message: string;
    variant: ToastVariant;
}

type Listener = (payload: ToastPayload) => void;

const listeners = new Set<Listener>();

/**
 * Module-level publisher. Any subscribed <Toast /> instance receives the
 * payload; a new showToast replaces the currently-visible toast (single
 * toast at a time by design).
 */
export function showToast(payload: ToastPayload): void {
    for (const fn of listeners) fn(payload);
}

const AUTO_DISMISS_MS = 3500;

function Toast() {
    const [current, setCurrent] = useState<ToastPayload | null>(null);
    const [seq, setSeq] = useState(0);

    useEffect(() => {
        const listener: Listener = (payload) => {
            setCurrent(payload);
            setSeq((n) => n + 1);
        };
        listeners.add(listener);
        return () => {
            listeners.delete(listener);
        };
    }, []);

    useEffect(() => {
        if (!current) return;
        const id = window.setTimeout(() => setCurrent(null), AUTO_DISMISS_MS);
        return () => window.clearTimeout(id);
    }, [current, seq]);

    if (!current) return null;

    return (
        <div
            key={seq}
            className={`app-toast app-toast--${current.variant}`}
            role="status"
            aria-live="polite"
        >
            <span className="app-toast__message">{current.message}</span>
            <button
                type="button"
                className="app-toast__dismiss"
                onClick={() => setCurrent(null)}
                aria-label="Dismiss notification"
            >
                ✕
            </button>
        </div>
    );
}

export default Toast;
