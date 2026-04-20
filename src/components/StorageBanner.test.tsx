import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";
import { render, screen, act, fireEvent } from "@testing-library/react";
import StorageBanner, { publishStorageFull } from "./StorageBanner";
import "../i18n";

describe("StorageBanner", () => {
    let infoSpy: ReturnType<typeof vi.spyOn>;
    beforeEach(() => {
        infoSpy = vi.spyOn(console, "info").mockImplementation(() => {});
    });
    afterEach(() => {
        infoSpy.mockRestore();
    });

    it("renders nothing until a storage-full event is published", () => {
        render(<StorageBanner />);
        expect(
            screen.queryByTestId("storage-banner"),
        ).not.toBeInTheDocument();
    });

    it("appears with localized title + resume action on storage-full publish", () => {
        render(<StorageBanner />);

        act(() => {
            publishStorageFull({
                path: "/tmp/session/transcript.jsonl",
                bytes_written: 0,
                bytes_lost: 4096,
            });
        });

        const banner = screen.getByTestId("storage-banner");
        expect(banner).toBeInTheDocument();
        expect(banner).toHaveAttribute("role", "alert");
        expect(
            screen.getByRole("button", { name: /resume/i }),
        ).toBeInTheDocument();
        // Message text comes from the en.json storage.message key.
        expect(
            screen.getByText(/capture paused/i),
        ).toBeInTheDocument();
    });

    it("hides when the dismiss (✕) button is clicked", () => {
        render(<StorageBanner />);
        act(() => {
            publishStorageFull({
                path: "/tmp/x",
                bytes_written: 0,
                bytes_lost: 1024,
            });
        });
        expect(screen.getByTestId("storage-banner")).toBeInTheDocument();

        act(() => {
            fireEvent.click(
                screen.getByRole("button", { name: /dismiss/i }),
            );
        });
        expect(
            screen.queryByTestId("storage-banner"),
        ).not.toBeInTheDocument();
    });

    it("hides and logs acknowledgement when Resume is clicked", () => {
        render(<StorageBanner />);
        act(() => {
            publishStorageFull({
                path: "/tmp/x",
                bytes_written: 0,
                bytes_lost: 1024,
            });
        });
        act(() => {
            fireEvent.click(screen.getByRole("button", { name: /resume/i }));
        });
        expect(
            screen.queryByTestId("storage-banner"),
        ).not.toBeInTheDocument();
        expect(infoSpy).toHaveBeenCalledWith(
            expect.stringContaining("acknowledged"),
        );
    });
});
