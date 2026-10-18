// Keyboard focus for the two pairing overlays: Tab stays inside, the page behind is inert and
// focus goes back to where it was. Closing keys are not handled here; each overlay owns its own.
import { useEffect, type RefObject } from "react";

// The index to focus on Tab (back false) or Shift+Tab (back true). current is -1 when the
// dialog itself, or something outside the list, has focus. -1 back means there is nothing to focus.
export function cycleFocus(count: number, current: number, back: boolean): number {
  if (count <= 0) return -1;
  if (current < 0) return back ? count - 1 : 0;
  if (back) return current === 0 ? count - 1 : current - 1;
  return current === count - 1 ? 0 : current + 1;
}

const FOCUSABLE = 'button, input, select, textarea, [href], [tabindex]:not([tabindex="-1"])';

function focusables(root: HTMLElement): HTMLElement[] {
  return Array.from(root.querySelectorAll<HTMLElement>(FOCUSABLE)).filter(
    (el) => !el.hasAttribute("disabled") && !el.closest("[inert]"),
  );
}

export function useModalFocus(ref: RefObject<HTMLElement | null>): void {
  useEffect(() => {
    const dialog = ref.current;
    if (!dialog) return;
    const opener = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    // The overlay wrapper is the dialog's parent; everything beside it is the page behind.
    const overlay = dialog.parentElement;
    const changed: Element[] = [];
    if (overlay?.parentElement) {
      for (const sibling of Array.from(overlay.parentElement.children)) {
        if (sibling !== overlay && !sibling.hasAttribute("inert")) {
          sibling.setAttribute("inert", "");
          changed.push(sibling);
        }
      }
    }
    dialog.focus();
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Tab") return;
      const list = focusables(dialog);
      const next = cycleFocus(list.length, list.indexOf(document.activeElement as HTMLElement), e.shiftKey);
      e.preventDefault();
      (next < 0 ? dialog : list[next]).focus();
    };
    dialog.addEventListener("keydown", onKey);
    return () => {
      dialog.removeEventListener("keydown", onKey);
      for (const el of changed) el.removeAttribute("inert");
      if (opener?.isConnected) opener.focus();
    };
    // Mount-only: the overlay lives exactly as long as the dialog does.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);
}
