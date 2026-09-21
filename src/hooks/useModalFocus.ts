import { useLayoutEffect, useRef } from "react";

const modalStack: HTMLElement[] = [];
const focusable = 'button:not(:disabled), input:not(:disabled):not([type="hidden"]), select:not(:disabled), textarea:not(:disabled), a[href], [tabindex]:not([tabindex="-1"])';

/** Own focus only while this is the top modal, including nested confirmations. */
export function useModalFocus() {
  const ref = useRef<HTMLDivElement>(null);
  const opener = useRef<HTMLElement | null>(document.activeElement as HTMLElement | null);
  useLayoutEffect(() => {
    const root = ref.current;
    if (!root) return;
    modalStack.push(root);
    const controls = () => Array.from(root.querySelectorAll<HTMLElement>(focusable))
      .filter((el) => !el.matches(":disabled") && !el.closest('[hidden], [inert]') && getComputedStyle(el).display !== "none" && getComputedStyle(el).visibility !== "hidden");
    const focusFirst = () => (controls()[0] ?? root).focus();
    if (!root.contains(document.activeElement)) focusFirst();
    function keydown(event: KeyboardEvent) {
      if (modalStack[modalStack.length - 1] !== root || event.key !== "Tab") return;
      const items = controls();
      const index = items.indexOf(document.activeElement as HTMLElement);
      if (!items.length || (event.shiftKey ? index <= 0 : index < 0 || index === items.length - 1)) {
        event.preventDefault();
        (event.shiftKey ? items[items.length - 1] ?? root : items[0] ?? root).focus();
      }
    }
    function focusin(event: FocusEvent) {
      if (modalStack[modalStack.length - 1] === root && !root.contains(event.target as Node)) focusFirst();
    }
    document.addEventListener("keydown", keydown);
    document.addEventListener("focusin", focusin);
    return () => {
      modalStack.splice(modalStack.indexOf(root), 1);
      document.removeEventListener("keydown", keydown);
      document.removeEventListener("focusin", focusin);
      if (opener.current?.isConnected) opener.current.focus();
    };
  }, []);
  return ref;
}
