import { describe, it, expect, afterEach } from "vitest";
import { renderHook } from "@testing-library/react";
import { useFocusTrap } from "./useFocusTrap";

// JSDOM has no layout engine, so offsetParent is always null. The hook uses
// `offsetParent !== null` as a cheap visibility filter in real browsers;
// stub it here to reflect "element is in the DOM tree".
const offsetParentDescriptor = Object.getOwnPropertyDescriptor(
    HTMLElement.prototype,
    "offsetParent",
);
Object.defineProperty(HTMLElement.prototype, "offsetParent", {
    configurable: true,
    get() {
        return this.parentNode;
    },
});

function resetDom() {
    while (document.body.firstChild) {
        document.body.removeChild(document.body.firstChild);
    }
}

function pressTab(shift = false) {
    const ev = new KeyboardEvent("keydown", {
        key: "Tab",
        shiftKey: shift,
        bubbles: true,
        cancelable: true,
    });
    document.dispatchEvent(ev);
    return ev;
}

// Silence unused-descriptor warning; it's kept so we can restore if needed.
void offsetParentDescriptor;

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

    describe("Tab cycling", () => {
        it("Tab on the last focusable child wraps to the first", () => {
            const first = makeButton("first");
            const middle = makeButton("middle");
            const last = makeButton("last");
            const container = appendAndFocus(
                makeContainer({ children: [first, middle, last] }),
            );

            renderHook(() => {
                const ref = useFocusTrap<HTMLDivElement>();
                ref.current = container;
                return ref;
            });

            last.focus();
            expect(document.activeElement).toBe(last);

            const ev = pressTab(false);
            expect(ev.defaultPrevented).toBe(true);
            expect(document.activeElement).toBe(first);
        });

        it("Shift+Tab on the first focusable child wraps to the last", () => {
            const first = makeButton("first");
            const middle = makeButton("middle");
            const last = makeButton("last");
            const container = appendAndFocus(
                makeContainer({ children: [first, middle, last] }),
            );

            renderHook(() => {
                const ref = useFocusTrap<HTMLDivElement>();
                ref.current = container;
                return ref;
            });

            first.focus();
            expect(document.activeElement).toBe(first);

            const ev = pressTab(true);
            expect(ev.defaultPrevented).toBe(true);
            expect(document.activeElement).toBe(last);
        });

        it("re-queries focusable children per event (dynamic additions)", () => {
            const first = makeButton("first");
            const container = appendAndFocus(makeContainer({ children: [first] }));

            renderHook(() => {
                const ref = useFocusTrap<HTMLDivElement>();
                ref.current = container;
                return ref;
            });

            first.focus();

            // Add a new focusable child after mount. Tab from old-last (first)
            // should now land on the newly-added last child, proving we
            // re-query per event instead of caching the initial list.
            const added = makeButton("added");
            container.appendChild(added);

            // first is now the "first", added is the "last". Tab from `added`
            // should wrap back to `first`.
            added.focus();
            const ev = pressTab(false);
            expect(ev.defaultPrevented).toBe(true);
            expect(document.activeElement).toBe(first);
        });

        it("is a no-op when the container has no focusable descendants", () => {
            const container = appendAndFocus(makeContainer({ tabIndex: -1 }));

            renderHook(() => {
                const ref = useFocusTrap<HTMLDivElement>();
                ref.current = container;
                return ref;
            });

            // Container is focused on mount. Tab inside an empty container
            // should prevent default (trap) but not throw.
            const ev = pressTab(false);
            expect(ev.defaultPrevented).toBe(true);
        });

        it("keeps focus on the sole focusable when only one exists", () => {
            const only = makeButton("only");
            const container = appendAndFocus(
                makeContainer({ children: [only] }),
            );

            renderHook(() => {
                const ref = useFocusTrap<HTMLDivElement>();
                ref.current = container;
                return ref;
            });

            only.focus();
            const ev = pressTab(false);
            expect(ev.defaultPrevented).toBe(true);
            expect(document.activeElement).toBe(only);

            const ev2 = pressTab(true);
            expect(ev2.defaultPrevented).toBe(true);
            expect(document.activeElement).toBe(only);
        });

        it("ignores non-Tab key presses", () => {
            const first = makeButton("first");
            const last = makeButton("last");
            const container = appendAndFocus(
                makeContainer({ children: [first, last] }),
            );

            renderHook(() => {
                const ref = useFocusTrap<HTMLDivElement>();
                ref.current = container;
                return ref;
            });

            last.focus();
            const ev = new KeyboardEvent("keydown", {
                key: "Enter",
                bubbles: true,
                cancelable: true,
            });
            document.dispatchEvent(ev);
            expect(ev.defaultPrevented).toBe(false);
            expect(document.activeElement).toBe(last);
        });
    });
});
