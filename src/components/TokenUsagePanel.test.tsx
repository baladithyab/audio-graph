import { describe, it, expect, beforeEach, vi } from "vitest";
import { render, screen, act, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { listen } from "@tauri-apps/api/event";
import TokenUsagePanel from "./TokenUsagePanel";
import "../i18n";
import type { GeminiStatusEvent } from "../types";

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

async function flushEffects() {
    // Let the async listen() promise resolve + React commit.
    await act(async () => {
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
        window.localStorage.clear();
    });

    it("shows empty state in both scopes before any usage arrives", () => {
        installListener();
        render(<TokenUsagePanel />);
        const emptyMessages = screen.getAllByText(/no token usage reported yet/i);
        expect(emptyMessages).toHaveLength(2);
    });

    it("accumulates totals across turn_complete events", async () => {
        const bus = installListener();
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

    it("hydrates session + lifetime from localStorage on mount", async () => {
        window.localStorage.setItem(
            SESSION_KEY,
            JSON.stringify({
                prompt: 77,
                response: 33,
                cached: 0,
                thoughts: 0,
                toolUse: 0,
                total: 110,
                turns: 2,
            }),
        );
        window.localStorage.setItem(
            LIFETIME_KEY,
            JSON.stringify({
                prompt: 500,
                response: 250,
                cached: 0,
                thoughts: 0,
                toolUse: 0,
                total: 800,
                turns: 10,
            }),
        );
        installListener();
        render(<TokenUsagePanel />);
        await flushEffects();

        const sessionTotal = within(sessionScope()).getByText("Total")
            .parentElement as HTMLElement;
        expect(sessionTotal).toHaveTextContent("110");

        const lifetimeTotal = within(lifetimeScope()).getByText("Total")
            .parentElement as HTMLElement;
        expect(lifetimeTotal).toHaveTextContent("800");
    });

    it("falls back to zero state when localStorage contains corrupt JSON", async () => {
        window.localStorage.setItem(SESSION_KEY, "{not valid json");
        window.localStorage.setItem(LIFETIME_KEY, "also garbage ]]]");
        installListener();
        render(<TokenUsagePanel />);
        await flushEffects();

        expect(
            screen.getAllByText(/no token usage reported yet/i),
        ).toHaveLength(2);
    });

    it("Clear All clears both session and lifetime when confirmed", async () => {
        const bus = installListener();
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
});
