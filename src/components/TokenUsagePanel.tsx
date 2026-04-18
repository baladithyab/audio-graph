import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { listen } from "@tauri-apps/api/event";
import type { GeminiStatusEvent, UsageMetadata } from "../types";

const GEMINI_STATUS = "gemini-status";
const SESSION_KEY = "tokens.session.v1";
const LIFETIME_KEY = "tokens.lifetime.v1";

interface Totals {
    prompt: number;
    response: number;
    cached: number;
    thoughts: number;
    toolUse: number;
    total: number;
    turns: number;
}

const ZERO_TOTALS: Totals = {
    prompt: 0,
    response: 0,
    cached: 0,
    thoughts: 0,
    toolUse: 0,
    total: 0,
    turns: 0,
};

function add(totals: Totals, u: UsageMetadata): Totals {
    return {
        prompt: totals.prompt + (u.promptTokenCount ?? 0),
        response: totals.response + (u.responseTokenCount ?? 0),
        cached: totals.cached + (u.cachedContentTokenCount ?? 0),
        thoughts: totals.thoughts + (u.thoughtsTokenCount ?? 0),
        toolUse: totals.toolUse + (u.toolUsePromptTokenCount ?? 0),
        total: totals.total + (u.totalTokenCount ?? 0),
        turns: totals.turns + 1,
    };
}

function formatCount(n: number): string {
    return n.toLocaleString();
}

function isFiniteNumber(v: unknown): v is number {
    return typeof v === "number" && Number.isFinite(v);
}

function parseTotals(raw: string | null): Totals {
    if (!raw) return ZERO_TOTALS;
    try {
        const parsed = JSON.parse(raw) as unknown;
        if (!parsed || typeof parsed !== "object") return ZERO_TOTALS;
        const p = parsed as Record<string, unknown>;
        const out: Totals = {
            prompt: isFiniteNumber(p.prompt) ? p.prompt : 0,
            response: isFiniteNumber(p.response) ? p.response : 0,
            cached: isFiniteNumber(p.cached) ? p.cached : 0,
            thoughts: isFiniteNumber(p.thoughts) ? p.thoughts : 0,
            toolUse: isFiniteNumber(p.toolUse) ? p.toolUse : 0,
            total: isFiniteNumber(p.total) ? p.total : 0,
            turns: isFiniteNumber(p.turns) ? p.turns : 0,
        };
        return out;
    } catch {
        return ZERO_TOTALS;
    }
}

function loadTotals(key: string): Totals {
    if (typeof window === "undefined" || !window.localStorage) return ZERO_TOTALS;
    try {
        return parseTotals(window.localStorage.getItem(key));
    } catch {
        return ZERO_TOTALS;
    }
}

function saveTotals(key: string, totals: Totals): void {
    if (typeof window === "undefined" || !window.localStorage) return;
    try {
        window.localStorage.setItem(key, JSON.stringify(totals));
    } catch {
        // Storage full or denied — silently ignore; in-memory state still works.
    }
}

function removeKey(key: string): void {
    if (typeof window === "undefined" || !window.localStorage) return;
    try {
        window.localStorage.removeItem(key);
    } catch {
        // ignore
    }
}

function TokenUsagePanel() {
    const { t } = useTranslation();
    const [session, setSession] = useState<Totals>(() => loadTotals(SESSION_KEY));
    const [lifetime, setLifetime] = useState<Totals>(() => loadTotals(LIFETIME_KEY));
    const [lastUsage, setLastUsage] = useState<UsageMetadata | null>(null);

    useEffect(() => {
        let unlisten: (() => void) | null = null;
        let cancelled = false;

        (async () => {
            const off = await listen<GeminiStatusEvent>(GEMINI_STATUS, (event) => {
                const payload = event.payload;
                if (payload.type !== "turn_complete" || !payload.usage) return;
                const usage = payload.usage as UsageMetadata;
                setSession((prev) => {
                    const next = add(prev, usage);
                    saveTotals(SESSION_KEY, next);
                    return next;
                });
                setLifetime((prev) => {
                    const next = add(prev, usage);
                    saveTotals(LIFETIME_KEY, next);
                    return next;
                });
                setLastUsage(usage);
            });
            if (cancelled) {
                off();
            } else {
                unlisten = off;
            }
        })();

        return () => {
            cancelled = true;
            if (unlisten) unlisten();
        };
    }, []);

    const handleReset = useCallback(() => {
        setSession(ZERO_TOTALS);
        setLastUsage(null);
        removeKey(SESSION_KEY);
    }, []);

    const handleClearAll = useCallback(() => {
        const confirmed =
            typeof window === "undefined"
                ? true
                : window.confirm(t("tokens.clearAllConfirm"));
        if (!confirmed) return;
        setSession(ZERO_TOTALS);
        setLifetime(ZERO_TOTALS);
        setLastUsage(null);
        removeKey(SESSION_KEY);
        removeKey(LIFETIME_KEY);
    }, [t]);

    const hasSession = session.turns > 0;
    const hasLifetime = lifetime.turns > 0;
    const hasAny = hasSession || hasLifetime;

    return (
        <section
            className="token-usage"
            aria-label={t("tokens.title")}
        >
            <div className="token-usage__header">
                <h3 className="panel-title">{t("tokens.title")}</h3>
                <div className="token-usage__header-actions">
                    {hasSession && (
                        <span
                            className="token-usage__turns"
                            title={t("tokens.turnsTooltip")}
                        >
                            {t("tokens.turns", { count: session.turns })}
                        </span>
                    )}
                    <button
                        type="button"
                        className="panel-export-btn"
                        onClick={handleReset}
                        disabled={!hasSession}
                        aria-label={t("tokens.reset")}
                        title={t("tokens.reset")}
                    >
                        {t("tokens.reset")}
                    </button>
                    <button
                        type="button"
                        className="panel-export-btn"
                        onClick={handleClearAll}
                        disabled={!hasAny}
                        aria-label={t("tokens.clearAll")}
                        title={t("tokens.clearAll")}
                    >
                        {t("tokens.clearAll")}
                    </button>
                </div>
            </div>

            <div
                className="token-usage__scope"
                aria-label={t("tokens.session")}
            >
                <h4 className="token-usage__scope-label">{t("tokens.session")}</h4>
                {!hasSession ? (
                    <p className="token-usage__empty">{t("tokens.empty")}</p>
                ) : (
                    <dl className="token-usage__grid">
                        <div className="token-usage__cell token-usage__cell--total">
                            <dt>{t("tokens.total")}</dt>
                            <dd>{formatCount(session.total)}</dd>
                        </div>
                        <div className="token-usage__cell">
                            <dt>{t("tokens.prompt")}</dt>
                            <dd>{formatCount(session.prompt)}</dd>
                        </div>
                        <div className="token-usage__cell">
                            <dt>{t("tokens.response")}</dt>
                            <dd>{formatCount(session.response)}</dd>
                        </div>
                        {session.thoughts > 0 && (
                            <div className="token-usage__cell">
                                <dt>{t("tokens.thoughts")}</dt>
                                <dd>{formatCount(session.thoughts)}</dd>
                            </div>
                        )}
                        {session.toolUse > 0 && (
                            <div className="token-usage__cell">
                                <dt>{t("tokens.toolUse")}</dt>
                                <dd>{formatCount(session.toolUse)}</dd>
                            </div>
                        )}
                        {session.cached > 0 && (
                            <div className="token-usage__cell">
                                <dt>{t("tokens.cached")}</dt>
                                <dd>{formatCount(session.cached)}</dd>
                            </div>
                        )}
                    </dl>
                )}
            </div>

            <div
                className="token-usage__scope token-usage__scope--lifetime"
                aria-label={t("tokens.lifetime")}
            >
                <h4 className="token-usage__scope-label">
                    {t("tokens.lifetime")}
                    {hasLifetime && (
                        <span
                            className="token-usage__scope-turns"
                            title={t("tokens.turnsTooltip")}
                        >
                            {t("tokens.turns", { count: lifetime.turns })}
                        </span>
                    )}
                </h4>
                {!hasLifetime ? (
                    <p className="token-usage__empty">{t("tokens.empty")}</p>
                ) : (
                    <dl className="token-usage__grid">
                        <div className="token-usage__cell token-usage__cell--total">
                            <dt>{t("tokens.total")}</dt>
                            <dd>{formatCount(lifetime.total)}</dd>
                        </div>
                        <div className="token-usage__cell">
                            <dt>{t("tokens.prompt")}</dt>
                            <dd>{formatCount(lifetime.prompt)}</dd>
                        </div>
                        <div className="token-usage__cell">
                            <dt>{t("tokens.response")}</dt>
                            <dd>{formatCount(lifetime.response)}</dd>
                        </div>
                        {lifetime.thoughts > 0 && (
                            <div className="token-usage__cell">
                                <dt>{t("tokens.thoughts")}</dt>
                                <dd>{formatCount(lifetime.thoughts)}</dd>
                            </div>
                        )}
                        {lifetime.toolUse > 0 && (
                            <div className="token-usage__cell">
                                <dt>{t("tokens.toolUse")}</dt>
                                <dd>{formatCount(lifetime.toolUse)}</dd>
                            </div>
                        )}
                        {lifetime.cached > 0 && (
                            <div className="token-usage__cell">
                                <dt>{t("tokens.cached")}</dt>
                                <dd>{formatCount(lifetime.cached)}</dd>
                            </div>
                        )}
                    </dl>
                )}
            </div>

            {lastUsage && (
                <p
                    className="token-usage__last"
                    title={t("tokens.lastTurnTooltip")}
                >
                    {t("tokens.lastTurn", {
                        total: formatCount(lastUsage.totalTokenCount ?? 0),
                    })}
                </p>
            )}
        </section>
    );
}

export default TokenUsagePanel;
