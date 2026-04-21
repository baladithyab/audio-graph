import { describe, it, expect, afterEach } from "vitest";
import { renderHook } from "@testing-library/react";
import { useFocusTrap } from "./useFocusTrap";

// Note: despite its name, useFocusTrap does NOT implement Tab-cycling
// inside a container (see the hook's own JSDoc). It's a narrower focus-
// in-on-mount + restore-on-unmount helper. Tests below mirror that
// contract exactly rather than the aspirational name.

function resetDom() {
    while (document.body.firstChild) {
        document.body.removeChild(document.body.firstChild);
    }
}

function appendAndFocus<T extends HTMLElement>(el: T): T {
    document.body.appendChild(el);
    return el;
}

function makeContainer(opts: {
    tabIndex?: number;
    children?: HTMLElement[];
}): HTMLDivElement {
    const div = document.createElement("div");
    if (opts.tabIndex !== undefined) {
        div.setAttribute("tabindex", String(opts.tabIndex));
    }
    for (const child of opts.children ?? []) {
        div.appendChild(child);
    }
    return div;
}

function makeButton(label: string): HTMLButtonElement {
    const btn = document.createElement("button");
    btn.textContent = label;
    return btn;
}

describe("useFocusTrap", () => {
    afterEach(() => {
        resetDom();
    });

    it("focuses the container itself when it is [tabindex]-focusable", () => {
        const opener = appendAndFocus(makeButton("opener"));
        opener.focus();

        const container = appendAndFocus(makeContainer({ tabIndex: -1 }));

        const { unmount } = renderHook(() => {
            const ref = useFocusTrap<HTMLDivElement>();
            ref.current = container;
            return ref;
        });

        expect(document.activeElement).toBe(container);
        unmount();
    });

    it("focuses the first focusable descendant when the container has no tabindex", () => {
        const opener = appendAndFocus(makeButton("opener"));
        opener.focus();

        const first = makeButton("first");
        const second = makeButton("second");
        const third = makeButton("third");
        const container = appendAndFocus(
            makeContainer({ children: [first, second, third] }),
        );

        renderHook(() => {
            const ref = useFocusTrap<HTMLDivElement>();
            ref.current = container;
            return ref;
        });

        expect(document.activeElement).toBe(first);
    });

    it("restores focus to the previously-focused element on unmount", () => {
        const opener = appendAndFocus(makeButton("opener"));
        opener.focus();
        expect(document.activeElement).toBe(opener);

        const container = appendAndFocus(makeContainer({ tabIndex: -1 }));

        const { unmount } = renderHook(() => {
            const ref = useFocusTrap<HTMLDivElement>();
            ref.current = container;
            return ref;
        });

        expect(document.activeElement).toBe(container);

        unmount();
        expect(document.activeElement).toBe(opener);
    });

    it("does not throw when the previously-focused element was removed from the DOM", () => {
        const opener = appendAndFocus(makeButton("opener"));
        opener.focus();

        const container = appendAndFocus(makeContainer({ tabIndex: -1 }));

        const { unmount } = renderHook(() => {
            const ref = useFocusTrap<HTMLDivElement>();
            ref.current = container;
            return ref;
        });

        // Opener is gone by the time the modal closes.
        opener.remove();

        expect(() => unmount()).not.toThrow();
    });

    it("is a no-op on mount when the ref is never assigned", () => {
        const opener = appendAndFocus(makeButton("opener"));
        opener.focus();

        renderHook(() => useFocusTrap<HTMLDivElement>());

        // No container → focus should remain on the opener.
        expect(document.activeElement).toBe(opener);
    });

    it("prefers the container itself over descendants when [tabindex] is set", () => {
        const opener = appendAndFocus(makeButton("opener"));
        opener.focus();

        const inner = makeButton("inner");
        const container = appendAndFocus(
            makeContainer({ tabIndex: -1, children: [inner] }),
        );

        renderHook(() => {
            const ref = useFocusTrap<HTMLDivElement>();
            ref.current = container;
            return ref;
        });

        // With a tabindex present, the hook focuses the container, not the
        // inner button — this is the behavior documented in the hook JSDoc
        // ("prefer it so screen readers announce the dialog's aria-labelledby").
        expect(document.activeElement).toBe(container);
        expect(document.activeElement).not.toBe(inner);
    });
});
