import { describe, it, expect, beforeEach, vi } from "vitest";
import { render, screen, act, within, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import TokenUsagePanel from "./TokenUsagePanel";
import "../i18n";
import type {
    GeminiStatusEvent,
    LifetimeUsage,
    SessionUsage,
} from "../types";

// The Tauri mock from src/test/setup.ts returns `() => {}` for listen.
// Override it here so we can capture and invoke the handler directly.
type Handler = (event: { payload: GeminiStatusEvent }) => void;

const SESSION_KEY = "tokens.session.v1";
const LIFETIME_KEY = "tokens.lifetime.v1";

function installListener() {
    const handlers: Handler[] = [];
    const mocked = listen as unknown as ReturnType<typeof vi.fn>;
    mocked.mockImplementation(
        async (_name: string, handler: Handler) => {
            handlers.push(handler);
            return () => {
                const idx = handlers.indexOf(handler);
                if (idx >= 0) handlers.splice(idx, 1);
            };
        },
    );
    return {
        emit(payload: GeminiStatusEvent) {
            for (const h of handlers) h({ payload });
        },
    };
}

const ZERO_SESSION: SessionUsage = {
    session_id: "test-session",
    prompt: 0,
    response: 0,
    cached: 0,
    thoughts: 0,
    tool_use: 0,
    total: 0,
    turns: 0,
    updated_at: 0,
};

const ZERO_LIFETIME: LifetimeUsage = {
    prompt: 0,
    response: 0,
    cached: 0,
    thoughts: 0,
    tool_use: 0,
    total: 0,
    turns: 0,
    sessions: 0,
};

/**
 * Install default mock `invoke` responses: zeroed session + lifetime so the
 * backend hydration path resolves without throwing. Individual tests can
 * still override `mockImplementation` beforehand (the mock is reset in
 * `beforeEach`).
 */
function installInvokeDefaults(overrides?: {
    session?: SessionUsage | Error;
    lifetime?: LifetimeUsage | Error;
    newSession?: string | Error;
}) {
    const mocked = invoke as unknown as ReturnType<typeof vi.fn>;
    mocked.mockImplementation(async (cmd: string) => {
        switch (cmd) {
            case "get_current_session_usage": {
                const v = overrides?.session ?? ZERO_SESSION;
                if (v instanceof Error) throw v;
                return v;
            }
            case "get_lifetime_usage": {
                const v = overrides?.lifetime ?? ZERO_LIFETIME;
                if (v instanceof Error) throw v;
                return v;
            }
            case "new_session_cmd": {
                const v = overrides?.newSession ?? "new-session-uuid";
                if (v instanceof Error) throw v;
                return v;
            }
            default:
                return undefined;
        }
    });
}

async function flushEffects() {
    // Let the async listen()/invoke() promises resolve + React commit.
    await act(async () => {
        await Promise.resolve();
        await Promise.resolve();
        await Promise.resolve();
        await Promise.resolve();
    });
}

function sessionScope(): HTMLElement {
    return screen.getByRole("region", { name: /gemini token usage/i })
        .querySelector('[aria-label="Session"]') as HTMLElement;
}

function lifetimeScope(): HTMLElement {
    return screen.getByRole("region", { name: /gemini token usage/i })
        .querySelector('[aria-label="Lifetime"]') as HTMLElement;
}

describe("TokenUsagePanel", () => {
    beforeEach(() => {
        (listen as unknown as ReturnType<typeof vi.fn>).mockReset();
        (invoke as unknown as ReturnType<typeof vi.fn>).mockReset();
        window.localStorage.clear();
    });

    it("shows empty state in both scopes before any usage arrives", async () => {
        installListener();
        installInvokeDefaults();
        render(<TokenUsagePanel />);
        await flushEffects();
        const emptyMessages = screen.getAllByText(/no token usage reported yet/i);
        expect(emptyMessages).toHaveLength(2);
    });

    it("accumulates totals across turn_complete events", async () => {
        const bus = installListener();
        installInvokeDefaults();
        render(<TokenUsagePanel />);
        await flushEffects();

        await act(async () => {
            bus.emit({
                type: "turn_complete",
                usage: {
                    promptTokenCount: 100,
                    responseTokenCount: 50,
                    totalTokenCount: 150,
                },
            });
        });
        await act(async () => {
            bus.emit({
                type: "turn_complete",
                usage: {
                    promptTokenCount: 40,
                    responseTokenCount: 10,
                    totalTokenCount: 50,
                    thoughtsTokenCount: 5,
                },
            });
        });

        const session = sessionScope();
        // Total row reflects sum across both turns (150 + 50 = 200).
        const totalCell = within(session).getByText("Total")
            .parentElement as HTMLElement;
        expect(totalCell).toHaveTextContent("200");

        // Prompt sums to 140.
        const promptCell = within(session).getByText("Prompt")
            .parentElement as HTMLElement;
        expect(promptCell).toHaveTextContent("140");

        // Thoughts only showed up on turn 2, sums to 5.
        const thoughtsCell = within(session).getByText("Thoughts")
            .parentElement as HTMLElement;
        expect(thoughtsCell).toHaveTextContent("5");
    });

    it("ignores turn_complete events without usage", async () => {
        const bus = installListener();
        installInvokeDefaults();
        render(<TokenUsagePanel />);
        await flushEffects();

        await act(async () => {
            bus.emit({ type: "turn_complete" });
        });

        expect(
            screen.getAllByText(/no token usage reported yet/i),
        ).toHaveLength(2);
    });

    it("ignores non-turn_complete status events", async () => {
        const bus = installListener();
        installInvokeDefaults();
        render(<TokenUsagePanel />);
        await flushEffects();

        await act(async () => {
            bus.emit({ type: "connected" });
            bus.emit({
                type: "error",
                message: "boom",
                usage: { promptTokenCount: 999, totalTokenCount: 999 },
            });
        });

        // Error payload with usage is NOT turn_complete, so it must be ignored.
        expect(
            screen.getAllByText(/no token usage reported yet/i),
        ).toHaveLength(2);
    });

    it("reset clears accumulated session totals", async () => {
        const bus = installListener();
        installInvokeDefaults();
        render(<TokenUsagePanel />);
        await flushEffects();

        await act(async () => {
            bus.emit({
                type: "turn_complete",
                usage: { totalTokenCount: 123, promptTokenCount: 100 },
            });
        });

        const session = sessionScope();
        const totalCell = within(session).getByText("Total")
            .parentElement as HTMLElement;
        expect(totalCell).toHaveTextContent("123");

        await userEvent.click(screen.getByRole("button", { name: /^reset$/i }));

        // Session empty, lifetime still holds the total.
        const sessionEmpty = within(sessionScope()).getByText(
            /no token usage reported yet/i,
        );
        expect(sessionEmpty).toBeInTheDocument();

        const lifetimeTotal = within(lifetimeScope()).getByText("Total")
            .parentElement as HTMLElement;
        expect(lifetimeTotal).toHaveTextContent("123");
    });

    it("persists session + lifetime to localStorage on turn_complete", async () => {
        const bus = installListener();
        installInvokeDefaults();
        render(<TokenUsagePanel />);
        await flushEffects();

        await act(async () => {
            bus.emit({
                type: "turn_complete",
                usage: {
                    promptTokenCount: 10,
                    responseTokenCount: 20,
                    totalTokenCount: 30,
                },
            });
        });

        const sessionRaw = window.localStorage.getItem(SESSION_KEY);
        const lifetimeRaw = window.localStorage.getItem(LIFETIME_KEY);
        expect(sessionRaw).not.toBeNull();
        expect(lifetimeRaw).not.toBeNull();

        const sessionParsed = JSON.parse(sessionRaw!);
        expect(sessionParsed).toMatchObject({
            prompt: 10,
            response: 20,
            total: 30,
            turns: 1,
        });
        const lifetimeParsed = JSON.parse(lifetimeRaw!);
        expect(lifetimeParsed).toMatchObject({
            prompt: 10,
            response: 20,
            total: 30,
            turns: 1,
        });
    });

    it("hydrates session + lifetime from backend on mount", async () => {
        installListener();
        installInvokeDefaults({
            session: {
                session_id: "live-session",
                prompt: 77,
                response: 33,
                cached: 0,
                thoughts: 0,
                tool_use: 0,
                total: 110,
                turns: 2,
                updated_at: 1_700_000_000_000,
            },
            lifetime: {
                prompt: 500,
                response: 250,
                cached: 0,
                thoughts: 0,
                tool_use: 0,
                total: 800,
                turns: 10,
                sessions: 4,
            },
        });
        render(<TokenUsagePanel />);
        await flushEffects();

        await waitFor(() => {
            const sessionTotal = within(sessionScope()).getByText("Total")
                .parentElement as HTMLElement;
            expect(sessionTotal).toHaveTextContent("110");
        });

        const lifetimeTotal = within(lifetimeScope()).getByText("Total")
            .parentElement as HTMLElement;
        expect(lifetimeTotal).toHaveTextContent("800");

        // Backend values are written through to localStorage so a
        // subsequent dev-mode reload (no Tauri) still has the last seen
        // state available.
        const sessionParsed = JSON.parse(window.localStorage.getItem(SESSION_KEY)!);
        expect(sessionParsed.total).toBe(110);
        expect(sessionParsed.turns).toBe(2);
        const lifetimeParsed = JSON.parse(window.localStorage.getItem(LIFETIME_KEY)!);
        expect(lifetimeParsed.total).toBe(800);
        expect(lifetimeParsed.turns).toBe(10);
    });

    it("falls back to localStorage when backend hydration fails", async () => {
        // Pre-seed localStorage as if a prior session wrote through.
        window.localStorage.setItem(
            SESSION_KEY,
            JSON.stringify({
                prompt: 11,
                response: 22,
                cached: 0,
                thoughts: 0,
                toolUse: 0,
                total: 33,
                turns: 1,
            }),
        );
        window.localStorage.setItem(
            LIFETIME_KEY,
            JSON.stringify({
                prompt: 100,
                response: 200,
                cached: 0,
                thoughts: 0,
                toolUse: 0,
                total: 300,
                turns: 5,
            }),
        );
        installListener();
        // Both backend commands reject — simulates browser dev mode (no Tauri)
        // or the backend not being ready. Component must keep the localStorage
        // values it hydrated from its initial render.
        installInvokeDefaults({
            session: new Error("no tauri"),
            lifetime: new Error("no tauri"),
        });
        render(<TokenUsagePanel />);
        await flushEffects();

        const sessionTotal = within(sessionScope()).getByText("Total")
            .parentElement as HTMLElement;
        expect(sessionTotal).toHaveTextContent("33");

        const lifetimeTotal = within(lifetimeScope()).getByText("Total")
            .parentElement as HTMLElement;
        expect(lifetimeTotal).toHaveTextContent("300");
    });

    it("falls back to zero state when localStorage contains corrupt JSON and backend fails", async () => {
        window.localStorage.setItem(SESSION_KEY, "{not valid json");
        window.localStorage.setItem(LIFETIME_KEY, "also garbage ]]]");
        installListener();
        installInvokeDefaults({
            session: new Error("no tauri"),
            lifetime: new Error("no tauri"),
        });
        render(<TokenUsagePanel />);
        await flushEffects();

        expect(
            screen.getAllByText(/no token usage reported yet/i),
        ).toHaveLength(2);
    });

    it("Clear All clears both session and lifetime when confirmed", async () => {
        const bus = installListener();
        installInvokeDefaults();
        const confirmSpy = vi
            .spyOn(window, "confirm")
            .mockReturnValue(true);

        render(<TokenUsagePanel />);
        await flushEffects();

        await act(async () => {
            bus.emit({
                type: "turn_complete",
                usage: { totalTokenCount: 500, promptTokenCount: 400 },
            });
        });

        expect(window.localStorage.getItem(SESSION_KEY)).not.toBeNull();
        expect(window.localStorage.getItem(LIFETIME_KEY)).not.toBeNull();

        await userEvent.click(
            screen.getByRole("button", { name: /clear all/i }),
        );

        expect(confirmSpy).toHaveBeenCalledTimes(1);
        expect(
            screen.getAllByText(/no token usage reported yet/i),
        ).toHaveLength(2);
        expect(window.localStorage.getItem(SESSION_KEY)).toBeNull();
        expect(window.localStorage.getItem(LIFETIME_KEY)).toBeNull();

        confirmSpy.mockRestore();
    });

    it("Clear All is a no-op when user cancels the confirm prompt", async () => {
        const bus = installListener();
        installInvokeDefaults();
        const confirmSpy = vi
            .spyOn(window, "confirm")
            .mockReturnValue(false);

        render(<TokenUsagePanel />);
        await flushEffects();

        await act(async () => {
            bus.emit({
                type: "turn_complete",
                usage: { totalTokenCount: 42, promptTokenCount: 20 },
            });
        });

        await userEvent.click(
            screen.getByRole("button", { name: /clear all/i }),
        );

        const lifetimeTotal = within(lifetimeScope()).getByText("Total")
            .parentElement as HTMLElement;
        expect(lifetimeTotal).toHaveTextContent("42");
        expect(window.localStorage.getItem(LIFETIME_KEY)).not.toBeNull();

        confirmSpy.mockRestore();
    });

    it("New Session finalizes the current session and re-hydrates from a fresh backend record", async () => {
        installListener();
        const mocked = invoke as unknown as ReturnType<typeof vi.fn>;
        // Three-phase behavior:
        //   1. Mount hydration returns a non-zero session (115 total / 2 turns).
        //   2. new_session_cmd returns a fresh UUID.
        //   3. Post-rotation get_current_session_usage returns a zeroed record
        //      for the new session.
        let postRotation = false;
        mocked.mockImplementation(async (cmd: string) => {
            switch (cmd) {
                case "get_current_session_usage":
                    if (postRotation) {
                        return {
                            session_id: "fresh-session",
                            prompt: 0,
                            response: 0,
                            cached: 0,
                            thoughts: 0,
                            tool_use: 0,
                            total: 0,
                            turns: 0,
                            updated_at: 0,
                        } satisfies SessionUsage;
                    }
                    return {
                        session_id: "old-session",
                        prompt: 80,
                        response: 35,
                        cached: 0,
                        thoughts: 0,
                        tool_use: 0,
                        total: 115,
                        turns: 2,
                        updated_at: 1_700_000_000_000,
                    } satisfies SessionUsage;
                case "get_lifetime_usage":
                    return {
                        prompt: 1000,
                        response: 500,
                        cached: 0,
                        thoughts: 0,
                        tool_use: 0,
                        total: 1500,
                        turns: 20,
                        sessions: 6,
                    } satisfies LifetimeUsage;
                case "new_session_cmd":
                    postRotation = true;
                    return "fresh-session";
                default:
                    return undefined;
            }
        });

        render(<TokenUsagePanel />);
        await flushEffects();

        // Initial hydration from backend.
        await waitFor(() => {
            const sessionTotal = within(sessionScope()).getByText("Total")
                .parentElement as HTMLElement;
            expect(sessionTotal).toHaveTextContent("115");
        });

        await userEvent.click(
            screen.getByRole("button", { name: /new session/i }),
        );

        // After rotation, Session panel shows empty; Lifetime is unchanged.
        await waitFor(() => {
            const sessionEmpty = within(sessionScope()).getByText(
                /no token usage reported yet/i,
            );
            expect(sessionEmpty).toBeInTheDocument();
        });
        const lifetimeTotal = within(lifetimeScope()).getByText("Total")
            .parentElement as HTMLElement;
        expect(lifetimeTotal).toHaveTextContent("1,500");

        expect(mocked).toHaveBeenCalledWith("new_session_cmd");
    });
});
