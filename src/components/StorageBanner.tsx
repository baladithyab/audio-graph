import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import type { CaptureStorageFullPayload } from "../types";

type Listener = (payload: CaptureStorageFullPayload) => void;

const listeners = new Set<Listener>();

export function publishStorageFull(payload: CaptureStorageFullPayload): void {
    for (const fn of listeners) fn(payload);
}

function StorageBanner() {
    const { t } = useTranslation();
    const [current, setCurrent] = useState<CaptureStorageFullPayload | null>(
        null,
    );

    useEffect(() => {
        const listener: Listener = (payload) => setCurrent(payload);
        listeners.add(listener);
        return () => {
            listeners.delete(listener);
        };
    }, []);

    if (!current) return null;

    const handleResume = () => {
        console.info("StorageBanner: user acknowledged storage-full, resuming");
        setCurrent(null);
    };
    const handleDismiss = () => setCurrent(null);

    return (
        <div
            className="storage-banner"
            role="alert"
            aria-live="assertive"
            data-testid="storage-banner"
        >
            <span className="storage-banner__icon" aria-hidden="true">
                ⚠
            </span>
            <div className="storage-banner__body">
                <strong className="storage-banner__title">
                    {t("storage.title")}
                </strong>
                <span className="storage-banner__message">
                    {t("storage.message")}
                </span>
            </div>
            <button
                type="button"
                className="storage-banner__resume"
                onClick={handleResume}
            >
                {t("storage.resume")}
            </button>
            <button
                type="button"
                className="storage-banner__dismiss"
                onClick={handleDismiss}
                aria-label={t("storage.dismiss")}
            >
                ✕
            </button>
        </div>
    );
}

export default StorageBanner;
