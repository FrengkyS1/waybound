import "@testing-library/jest-dom/vitest";
import { clearMocks, mockIPC } from "@tauri-apps/api/mocks";
import { afterEach, beforeEach, vi } from "vitest";

// `installStore.ts` and `play/store.ts` both call `listen(...)` at module
// scope, so merely importing anything that pulls them in reaches for the
// Tauri IPC bridge before a test has had any chance to set one up. A
// permissive baseline mock keeps that import-time work from throwing;
// individual tests still override it with their own `mockIPC` when they
// care what a command returns.
beforeEach(() => {
  mockIPC(() => undefined);
});

afterEach(() => {
  clearMocks();
  vi.restoreAllMocks();
});
