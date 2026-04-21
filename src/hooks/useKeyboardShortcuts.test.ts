import { describe, it, expect, beforeEach, vi } from "vitest";
import { renderHook, act } from "@testing-library/react";
import { fireEvent } from "@testing-library/react";
import { useKeyboardShortcuts } from "./useKeyboardShortcuts";
import { useAudioGraphStore } from "../store";

// The store's openSettings/openSessionsBrowser internally invoke Tauri
// commands to hydrate content; those are mocked to noop via src/test/setup.ts.
// We only care here about the boolean flags and capture toggling.

function resetStore() {
    useAudioGraphStore.setState({
        settingsOpen: false,
        sessionsBrowserOpen: false,
        isCapturing: false,
        selectedSourceIds: ["mic-1"],
        error: null,
    });
}

describe("useKeyboardShortcuts", () => {
    beforeEach(() => {
        resetStore();
    });

    it("Cmd+R toggles capture on when not capturing", () => {
        const startCapture = vi.fn();
        useAudioGraphStore.setState({ startCapture, isCapturing: false });

        renderHook(() => useKeyboardShortcuts());

        act(() => {
            fireEvent.keyDown(window, { key: "r", metaKey: true });
        });

        expect(startCapture).toHaveBeenCalledTimes(1);
    });

    it("Ctrl+R toggles capture off when currently capturing", () => {
        const stopCapture = vi.fn();
        useAudioGraphStore.setState({ stopCapture, isCapturing: true });

        renderHook(() => useKeyboardShortcuts());

        act(() => {
            fireEvent.keyDown(window, { key: "R", ctrlKey: true });
        });

        expect(stopCapture).toHaveBeenCalledTimes(1);
    });

    it("does NOT fire Cmd+R without any modifier", () => {
        const startCapture = vi.fn();
        useAudioGraphStore.setState({ startCapture });

        renderHook(() => useKeyboardShortcuts());

        act(() => {
            fireEvent.keyDown(window, { key: "r" });
        });

        expect(startCapture).not.toHaveBeenCalled();
    });

    it("Cmd+, opens the settings modal", () => {
        const openSettings = vi.fn();
        useAudioGraphStore.setState({ openSettings });

        renderHook(() => useKeyboardShortcuts());

        act(() => {
            fireEvent.keyDown(window, { key: ",", metaKey: true });
        });

        expect(openSettings).toHaveBeenCalledTimes(1);
    });

    it("Cmd+Shift+S opens sessions browser (not plain Cmd+S)", () => {
        const openSessionsBrowser = vi.fn();
        const startCapture = vi.fn();
        useAudioGraphStore.setState({ openSessionsBrowser, startCapture });

        renderHook(() => useKeyboardShortcuts());

        // Plain Cmd+S should not trigger either handler (no binding for it).
        act(() => {
            fireEvent.keyDown(window, { key: "s", metaKey: true });
        });
        expect(openSessionsBrowser).not.toHaveBeenCalled();
        expect(startCapture).not.toHaveBeenCalled();

        // Cmd+Shift+S opens sessions browser.
        act(() => {
            fireEvent.keyDown(window, {
                key: "S",
                metaKey: true,
                shiftKey: true,
            });
        });
        expect(openSessionsBrowser).toHaveBeenCalledTimes(1);
    });

    it("skips modifier shortcuts when focus is inside an <input>", () => {
        const startCapture = vi.fn();
        useAudioGraphStore.setState({ startCapture });

        renderHook(() => useKeyboardShortcuts());

        const input = document.createElement("input");
        document.body.appendChild(input);
        input.focus();

        act(() => {
            fireEvent.keyDown(input, { key: "r", metaKey: true });
        });

        expect(startCapture).not.toHaveBeenCalled();
        document.body.removeChild(input);
    });

    it("Escape closes settings modal even when typing in an input", () => {
        const closeSettings = vi.fn();
        useAudioGraphStore.setState({ closeSettings, settingsOpen: true });

        renderHook(() => useKeyboardShortcuts());

        const input = document.createElement("input");
        document.body.appendChild(input);
        input.focus();

        act(() => {
            fireEvent.keyDown(input, { key: "Escape" });
        });

        expect(closeSettings).toHaveBeenCalledTimes(1);
        document.body.removeChild(input);
    });

    it("Escape closes sessions browser when it is the open modal", () => {
        const closeSessionsBrowser = vi.fn();
        const closeSettings = vi.fn();
        useAudioGraphStore.setState({
            closeSessionsBrowser,
            closeSettings,
            settingsOpen: false,
            sessionsBrowserOpen: true,
        });

        renderHook(() => useKeyboardShortcuts());

        act(() => {
            fireEvent.keyDown(window, { key: "Escape" });
        });

        expect(closeSessionsBrowser).toHaveBeenCalledTimes(1);
        expect(closeSettings).not.toHaveBeenCalled();
    });

    it("Escape is a no-op when no modal is open", () => {
        const closeSettings = vi.fn();
        const closeSessionsBrowser = vi.fn();
        useAudioGraphStore.setState({
            closeSettings,
            closeSessionsBrowser,
            settingsOpen: false,
            sessionsBrowserOpen: false,
        });

        renderHook(() => useKeyboardShortcuts());

        act(() => {
            fireEvent.keyDown(window, { key: "Escape" });
        });

        expect(closeSettings).not.toHaveBeenCalled();
        expect(closeSessionsBrowser).not.toHaveBeenCalled();
    });

    it("removes its keydown listener on unmount", () => {
        const startCapture = vi.fn();
        useAudioGraphStore.setState({ startCapture });

        const { unmount } = renderHook(() => useKeyboardShortcuts());
        unmount();

        act(() => {
            fireEvent.keyDown(window, { key: "r", metaKey: true });
        });

        expect(startCapture).not.toHaveBeenCalled();
    });
});
