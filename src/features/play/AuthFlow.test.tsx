import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import type * as PlayApi from "./api";
const mocks = vi.hoisted(() => ({
  login: vi.fn(), cancel: vi.fn(), stop: vi.fn(),
  listeners: new Map<string, (event: { payload: unknown }) => void>(),
}));
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async (name: string, handler: (event: { payload: unknown }) => void) => {
    mocks.listeners.set(name, handler);
    return () => mocks.listeners.delete(name);
  }),
}));
vi.mock("@tauri-apps/plugin-opener", () => ({ openUrl: vi.fn(async () => {}) }));
vi.mock("./api", async (original) => ({
  ...await original<typeof PlayApi>(),
  microsoftLogin: mocks.login,
  cancelMicrosoftLogin: mocks.cancel,
  stopGame: mocks.stop,
  getAccount: vi.fn(async () => null),
  getRunningInstances: vi.fn(async () => []),
}));
import { usePlayStore, type LaunchState } from "./store";
import { SignInDialog } from "./SignInDialog";
import { LaunchOverlay } from "./LaunchOverlay";

const prompt = { userCode: "ABCD1234", verificationUri: "https://login.live.com", message: "Enter code" };
function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason: unknown) => void;
  const promise = new Promise<T>((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
}
function run(phase: LaunchState["phase"], startedAtMs: number): LaunchState {
  return { instanceId: "one", instanceName: "One", phase, startedAtMs, stage: "", current: 0,
    total: 0, logs: [], exitCode: null, error: null, crashed: false, crashReason: null };
}

beforeEach(() => {
  mocks.login.mockReset();
  mocks.cancel.mockReset().mockResolvedValue(undefined);
  mocks.stop.mockReset().mockResolvedValue(undefined);
  usePlayStore.setState({ account: null, signingIn: false, devicePrompt: null, launches: {}, launchDockMinimized: false });
});

describe("device sign-in lifecycle", () => {
  it("clears failed code, exposes retry, and starts a fresh attempt", async () => {
    const first = deferred<{ uuid: string; username: string }>();
    const second = deferred<{ uuid: string; username: string }>();
    mocks.login.mockReturnValueOnce(first.promise).mockReturnValueOnce(second.promise);
    await usePlayStore.getState().init();
    const onSignedIn = vi.fn();
    render(<SignInDialog onClose={vi.fn()} onSignedIn={onSignedIn} />);
    fireEvent.click(screen.getByRole("button", { name: "Get device code & sign in" }));
    const firstId = mocks.login.mock.calls[0][0];
    act(() => mocks.listeners.get("auth://device-code")?.({ payload: { loginId: firstId, prompt } }));
    expect(screen.getByText(prompt.userCode)).toBeInTheDocument();
    await act(async () => first.reject(new Error("Code expired")));
    expect(screen.queryByText(prompt.userCode)).not.toBeInTheDocument();
    expect(screen.getByRole("alert")).toHaveTextContent("Code expired");
    fireEvent.click(screen.getByRole("button", { name: "Retry sign-in" }));
    expect(mocks.login.mock.calls[1][0]).not.toBe(firstId);
    act(() => mocks.listeners.get("auth://device-code")?.({ payload: { loginId: firstId, prompt } }));
    expect(screen.queryByText(prompt.userCode)).not.toBeInTheDocument();
    await act(async () => second.resolve({ uuid: "uuid", username: "Player" }));
    expect(onSignedIn).toHaveBeenCalledOnce();
  });

  it("cancels by attempt ID and ignores completion after dismissal", async () => {
    const login = deferred<{ uuid: string; username: string }>();
    mocks.login.mockReturnValue(login.promise);
    const onClose = vi.fn();
    const onSignedIn = vi.fn();
    render(<SignInDialog onClose={onClose} onSignedIn={onSignedIn} />);
    fireEvent.click(screen.getByRole("button", { name: "Get device code & sign in" }));
    fireEvent.click(screen.getByRole("button", { name: "Cancel sign-in" }));
    await waitFor(() => expect(onClose).toHaveBeenCalledOnce());
    expect(mocks.cancel).toHaveBeenCalledWith(mocks.login.mock.calls[0][0]);
    await act(async () => login.resolve({ uuid: "uuid", username: "Player" }));
    expect(onSignedIn).not.toHaveBeenCalled();
    expect(usePlayStore.getState().account).toBeNull();
    expect(usePlayStore.getState().devicePrompt).toBeNull();
    expect(usePlayStore.getState().signingIn).toBe(false);
  });
});

it("re-arms dock Stop for each run of the same instance", () => {
  usePlayStore.setState({ launches: { one: run("running", 1) } });
  render(<LaunchOverlay />);
  fireEvent.click(screen.getByRole("button", { name: "Stop" }));
  expect(screen.getByRole("button", { name: "Stopping…" })).toBeDisabled();
  act(() => usePlayStore.setState({ launches: { one: run("exited", 1) } }));
  act(() => usePlayStore.setState({ launches: { one: run("running", 2) } }));
  fireEvent.click(screen.getByRole("button", { name: "Stop" }));
  expect(mocks.stop).toHaveBeenCalledTimes(2);
});
