import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";
import { render, screen, act, fireEvent } from "@testing-library/react";
import Toast, { showToast } from "./Toast";

describe("Toast", () => {
    beforeEach(() => {
        vi.useFakeTimers();
    });
    afterEach(() => {
        vi.useRealTimers();
    });

    it("renders nothing until showToast is called", () => {
        render(<Toast />);
        expect(screen.queryByRole("status")).not.toBeInTheDocument();
    });

    it("displays a success-variant message then auto-dismisses", async () => {
        render(<Toast />);

        act(() => {
            showToast({ variant: "success", message: "Session resumed" });
        });

        const el = screen.getByRole("status");
        expect(el).toHaveTextContent("Session resumed");
        expect(el.className).toContain("app-toast--success");

        act(() => {
            vi.advanceTimersByTime(4000);
        });
        expect(screen.queryByRole("status")).not.toBeInTheDocument();
    });

    it("replaces the visible toast when a new one fires before dismiss", () => {
        render(<Toast />);

        act(() => {
            showToast({ variant: "success", message: "First" });
        });
        expect(screen.getByRole("status")).toHaveTextContent("First");

        act(() => {
            vi.advanceTimersByTime(1000);
            showToast({ variant: "info", message: "Second" });
        });

        const el = screen.getByRole("status");
        expect(el).toHaveTextContent("Second");
        expect(el.className).toContain("app-toast--info");
        // Only one toast visible at a time.
        expect(screen.getAllByRole("status")).toHaveLength(1);
    });

    it("can be dismissed manually via the close button", () => {
        render(<Toast />);

        act(() => {
            showToast({ variant: "info", message: "Reconnected with fresh session" });
        });

        act(() => {
            fireEvent.click(screen.getByRole("button", { name: /dismiss/i }));
        });
        expect(screen.queryByRole("status")).not.toBeInTheDocument();
    });
});
