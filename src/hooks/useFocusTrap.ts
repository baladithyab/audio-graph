import { useEffect, useRef } from "react";

const FOCUSABLE_SELECTOR = [
  "a[href]",
  "area[href]",
  "button:not([disabled])",
  "embed",
  "iframe",
  "input:not([disabled])",
  "object",
  "select:not([disabled])",
  "textarea:not([disabled])",
  '[tabindex]:not([tabindex="-1"])',
].join(",");

function getVisibleFocusable(container: HTMLElement): HTMLElement[] {
  const nodes = container.querySelectorAll<HTMLElement>(FOCUSABLE_SELECTOR);
  return Array.from(nodes).filter((el) => el.offsetParent !== null);
}

/**
 * Focus trap for modal dialogs.
 *
 * On mount: remembers the currently-focused element (the thing that opened
 * the modal) and moves focus into the modal container — preferring the
 * container itself if it's focusable (e.g. `tabIndex={-1}`), otherwise
 * falling back to the first focusable descendant.
 *
 * While mounted: intercepts Tab / Shift+Tab at the container boundary and
 * cycles focus between the first and last focusable descendants. Focusables
 * are re-queried on every keypress so dynamically-added children work.
 *
 * On unmount: restores focus to whatever was focused before the modal opened,
 * so keyboard users land back where they started.
 *
 * Usage:
 *   const ref = useFocusTrap<HTMLDivElement>();
 *   return <div ref={ref} role="dialog" tabIndex={-1}>…</div>;
 */
export function useFocusTrap<T extends HTMLElement = HTMLElement>() {
  const containerRef = useRef<T | null>(null);
  const previouslyFocusedRef = useRef<HTMLElement | null>(null);

  useEffect(() => {
    previouslyFocusedRef.current =
      (document.activeElement as HTMLElement | null) ?? null;

    const el = containerRef.current;
    if (el) {
      const hasTabIndex = el.hasAttribute("tabindex");
      if (hasTabIndex) {
        el.focus();
      } else {
        const focusable = el.querySelector<HTMLElement>(FOCUSABLE_SELECTOR);
        focusable?.focus();
      }
    }

    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key !== "Tab") return;
      const container = containerRef.current;
      if (!container) return;

      const focusable = getVisibleFocusable(container);
      if (focusable.length === 0) {
        event.preventDefault();
        return;
      }
      if (focusable.length === 1) {
        event.preventDefault();
        focusable[0].focus();
        return;
      }

      const first = focusable[0];
      const last = focusable[focusable.length - 1];
      const active = document.activeElement as HTMLElement | null;

      if (event.shiftKey) {
        if (active === first || !container.contains(active)) {
          event.preventDefault();
          last.focus();
        }
      } else {
        if (active === last || !container.contains(active)) {
          event.preventDefault();
          first.focus();
        }
      }
    };

    document.addEventListener("keydown", onKeyDown, true);

    return () => {
      document.removeEventListener("keydown", onKeyDown, true);

      const prev = previouslyFocusedRef.current;
      if (prev && typeof prev.focus === "function" && document.contains(prev)) {
        prev.focus();
      }
    };
  }, []);

  return containerRef;
}
