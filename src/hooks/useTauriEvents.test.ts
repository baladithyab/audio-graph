import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";
import { renderHook, waitFor } from "@testing-library/react";
import { listen } from "@tauri-apps/api/event";
import type { Event } from "@tauri-apps/api/event";
import { useTauriEvents } from "./useTauriEvents";
import { useAudioGraphStore } from "../store";

// The global setup (src/test/setup.ts) already mocks @tauri-apps/api/event
// with a `listen` that returns a no-op unlisten. Here we redefine its
// behavior per-test so we can capture handlers and assert payload routing.
type Handler = (event: Event<unknown>) => void;

function makeEvent<T>(name: string, payload: T): Event<T> {
    return { event: name, id: 0, payload } as Event<T>;
}

function resetStore() {
    useAudioGraphStore.setState({
        transcriptSegments: [],
        graphSnapshot: {
            nodes: [],
            links: [],
            stats: { total_nodes: 0, total_edges: 0, total_episodes: 0 },
        },
        pipelineStatus: {
            capture: { type: "Idle" },
            pipeline: { type: "Idle" },
            asr: { type: "Idle" },
            diarization: { type: "Idle" },
            entity_extraction: { type: "Idle" },
            graph: { type: "Idle" },
        },
        speakers: [],
        backpressuredSources: [],
        geminiTranscripts: [],
        error: null,
        isGeminiActive: true,
    });
}

describe("useTauriEvents", () => {
    const handlers = new Map<string, Handler>();
    const unlisteners: Array<ReturnType<typeof vi.fn>> = [];

    beforeEach(() => {
        handlers.clear();
        unlisteners.length = 0;
        resetStore();

        vi.mocked(listen).mockImplementation(
            async (eventName: string, cb: Handler) => {
                handlers.set(eventName, cb);
                const unlisten = vi.fn();
                unlisteners.push(unlisten);
                return unlisten;
            },
        );
    });

    afterEach(() => {
        vi.clearAllMocks();
    });

    // The hook's setup() body chains 10 sequential `await listen(...)` calls,
    // so we need enough microtask ticks for all of them to register before
    // asserting. `waitFor` polls until all handlers are present.
    async function waitForAllHandlers() {
        await waitFor(() => {
            expect(handlers.size).toBe(10);
        });
    }

    it("subscribes to all expected events on mount", async () => {
        const { unmount } = renderHook(() => useTauriEvents());
        await waitForAllHandlers();

        const expected = [
            "transcript-update",
            "graph-update",
            "pipeline-status",
            "speaker-detected",
            "capture-error",
            "capture-backpressure",
            "capture-storage-full",
            "gemini-transcription",
            "gemini-response",
            "gemini-status",
        ];
        for (const name of expected) {
            expect(handlers.has(name)).toBe(true);
        }
        expect(handlers.size).toBe(expected.length);
        unmount();
    });

    it("invokes every registered unlisten on unmount", async () => {
        const { unmount } = renderHook(() => useTauriEvents());
        await waitForAllHandlers();

        const count = unlisteners.length;
        expect(count).toBe(10);
        unmount();

        for (const fn of unlisteners) {
            expect(fn).toHaveBeenCalledTimes(1);
        }
    });

    it("routes transcript-update payload into the store", async () => {
        renderHook(() => useTauriEvents());
        await waitForAllHandlers();

        const segment = {
            id: "seg-1",
            speaker_id: "spk-1",
            text: "hello",
            start_time: 0,
            end_time: 1,
            confidence: 0.9,
        };
        handlers.get("transcript-update")?.(
            makeEvent("transcript-update", segment),
        );

        expect(useAudioGraphStore.getState().transcriptSegments).toEqual([
            segment,
        ]);
    });

    it("routes pipeline-status and speaker-detected payloads", async () => {
        renderHook(() => useTauriEvents());
        await waitForAllHandlers();

        const running = { type: "Running" } as const;
        const status = {
            capture: running,
            pipeline: running,
            asr: running,
            diarization: running,
            entity_extraction: running,
            graph: running,
        };
        handlers.get("pipeline-status")?.(makeEvent("pipeline-status", status));
        expect(useAudioGraphStore.getState().pipelineStatus).toEqual(status);

        const speaker = { id: "spk-1", label: "Alice", color: "#ff0000" };
        handlers.get("speaker-detected")?.(
            makeEvent("speaker-detected", speaker),
        );
        expect(useAudioGraphStore.getState().speakers).toContainEqual(speaker);
    });

    it("sets store.error from capture-error payload", async () => {
        const errSpy = vi.spyOn(console, "error").mockImplementation(() => {});
        renderHook(() => useTauriEvents());
        await waitForAllHandlers();

        handlers.get("capture-error")?.(
            makeEvent("capture-error", {
                source_id: "mic-1",
                error: "device disconnected",
            }),
        );

        expect(useAudioGraphStore.getState().error).toBe("device disconnected");
        errSpy.mockRestore();
    });

    it("tracks capture-backpressure add and clear transitions", async () => {
        renderHook(() => useTauriEvents());
        await waitForAllHandlers();

        handlers.get("capture-backpressure")?.(
            makeEvent("capture-backpressure", {
                source_id: "mic-1",
                is_backpressured: true,
            }),
        );
        expect(useAudioGraphStore.getState().backpressuredSources).toContain(
            "mic-1",
        );

        handlers.get("capture-backpressure")?.(
            makeEvent("capture-backpressure", {
                source_id: "mic-1",
                is_backpressured: false,
            }),
        );
        expect(useAudioGraphStore.getState().backpressuredSources).not.toContain(
            "mic-1",
        );
    });

    it("appends gemini-transcription events to the transcript list", async () => {
        renderHook(() => useTauriEvents());
        await waitForAllHandlers();

        handlers.get("gemini-transcription")?.(
            makeEvent("gemini-transcription", {
                text: "hi there",
                is_final: true,
            }),
        );

        const entries = useAudioGraphStore.getState().geminiTranscripts;
        expect(entries).toHaveLength(1);
        expect(entries[0]).toMatchObject({
            text: "hi there",
            is_final: true,
            source: "gemini",
        });
    });

    it("flips isGeminiActive off when gemini-status 'disconnected' fires", async () => {
        renderHook(() => useTauriEvents());
        await waitForAllHandlers();
        expect(useAudioGraphStore.getState().isGeminiActive).toBe(true);

        handlers.get("gemini-status")?.(
            makeEvent("gemini-status", { type: "disconnected" }),
        );
        expect(useAudioGraphStore.getState().isGeminiActive).toBe(false);
    });
});
