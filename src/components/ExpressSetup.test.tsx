import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, fireEvent, act } from "@testing-library/react";
import ExpressSetup from "./ExpressSetup";
import "../i18n";
import { invoke } from "@tauri-apps/api/core";

const mockedInvoke = vi.mocked(invoke);

describe("ExpressSetup", () => {
    beforeEach(() => {
        mockedInvoke.mockReset();
        // Default: any save_* command succeeds; load returns null.
        mockedInvoke.mockImplementation(async (cmd: string) => {
            if (cmd === "load_credential_cmd") return null;
            return undefined;
        });
    });

    it("renders the quickstart dialog with ASR and LLM provider selectors", () => {
        render(
            <ExpressSetup onDismiss={() => {}} onOpenAdvanced={() => {}} />,
        );
        expect(
            screen.getByRole("dialog", { name: /quick setup/i }),
        ).toBeInTheDocument();
        // ASR and LLM dropdowns are present and default to a cloud provider
        // so the API-key field is visible.
        expect(screen.getByLabelText(/ASR/i)).toBeInTheDocument();
        expect(screen.getByLabelText(/LLM/i)).toBeInTheDocument();
        // Both cloud providers need a key → there are two API key inputs.
        expect(screen.getAllByLabelText(/API Key/i)).toHaveLength(2);
    });

    it("hides the API key input when Local Whisper is selected for ASR", () => {
        render(
            <ExpressSetup onDismiss={() => {}} onOpenAdvanced={() => {}} />,
        );
        const asrSelect = screen.getByLabelText(/ASR/i) as HTMLSelectElement;
        fireEvent.change(asrSelect, { target: { value: "local_whisper" } });
        // Now only the LLM (default OpenAI, still cloud) shows a key input.
        expect(screen.getAllByLabelText(/API Key/i)).toHaveLength(1);
    });

    it("disables Save & Start until required cloud keys are filled", () => {
        render(
            <ExpressSetup onDismiss={() => {}} onOpenAdvanced={() => {}} />,
        );
        const save = screen.getByRole("button", { name: /save & start/i });
        expect(save).toBeDisabled();

        // Fill ASR key (Gemini by default).
        const asrKey = screen.getAllByLabelText(/API Key/i)[0];
        fireEvent.change(asrKey, { target: { value: "gemini-key-123" } });
        expect(save).toBeDisabled(); // LLM still missing.

        const llmKey = screen.getAllByLabelText(/API Key/i)[1];
        fireEvent.change(llmKey, { target: { value: "sk-openai-abc" } });
        expect(save).toBeEnabled();
    });

    it("saves credentials + settings and dismisses when Save & Start is clicked", async () => {
        const onDismiss = vi.fn();
        render(
            <ExpressSetup onDismiss={onDismiss} onOpenAdvanced={() => {}} />,
        );

        // Switch to Deepgram so we can assert the deepgram_api_key slot.
        const asrSelect = screen.getByLabelText(/ASR/i) as HTMLSelectElement;
        fireEvent.change(asrSelect, { target: { value: "deepgram" } });

        const asrKey = screen.getAllByLabelText(/API Key/i)[0];
        fireEvent.change(asrKey, { target: { value: "dg-key" } });

        const llmKey = screen.getAllByLabelText(/API Key/i)[1];
        fireEvent.change(llmKey, { target: { value: "sk-openai" } });

        await act(async () => {
            fireEvent.click(
                screen.getByRole("button", { name: /save & start/i }),
            );
        });

        // We expect save_credential_cmd for Deepgram + OpenAI, and a
        // save_settings_cmd containing the Deepgram ASR provider.
        const creds = mockedInvoke.mock.calls.filter(
            ([cmd]) => cmd === "save_credential_cmd",
        );
        const credKeys = creds.map(([, args]) => (args as { key: string }).key);
        expect(credKeys).toContain("deepgram_api_key");
        expect(credKeys).toContain("openai_api_key");

        const saveSettings = mockedInvoke.mock.calls.find(
            ([cmd]) => cmd === "save_settings_cmd",
        );
        expect(saveSettings).toBeTruthy();
        const settingsArg = (saveSettings![1] as { settings: { asr_provider: { type: string } } })
            .settings;
        expect(settingsArg.asr_provider.type).toBe("deepgram");

        expect(onDismiss).toHaveBeenCalled();
    });

    it("dismisses without saving on Skip setup and on Escape", () => {
        const onDismiss = vi.fn();
        const { unmount } = render(
            <ExpressSetup onDismiss={onDismiss} onOpenAdvanced={() => {}} />,
        );
        // Two elements have "skip setup" accessible names (header ✕ and
        // footer button). The footer button is the user-visible text-only one.
        const skipButtons = screen.getAllByRole("button", {
            name: /skip setup/i,
        });
        const footerSkip = skipButtons.find(
            (b) => b.textContent?.trim() === "Skip setup",
        );
        fireEvent.click(footerSkip!);
        expect(onDismiss).toHaveBeenCalledTimes(1);
        expect(
            mockedInvoke.mock.calls.filter(
                ([cmd]) => cmd === "save_credential_cmd",
            ),
        ).toHaveLength(0);

        unmount();
        const onDismiss2 = vi.fn();
        render(
            <ExpressSetup onDismiss={onDismiss2} onOpenAdvanced={() => {}} />,
        );
        fireEvent.keyDown(window, { key: "Escape" });
        expect(onDismiss2).toHaveBeenCalledTimes(1);
    });
});
