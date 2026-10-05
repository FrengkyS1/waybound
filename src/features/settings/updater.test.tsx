import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import type { DownloadEvent, Update } from "@tauri-apps/plugin-updater";
import type { UpdateStatus } from "./updater";
import { StrictMode } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const { check } = vi.hoisted(() => ({ check: vi.fn() }));
vi.mock("@tauri-apps/plugin-updater", () => ({ check }));
vi.mock("@tauri-apps/api/app", () => ({ getVersion: async () => "0.1.6" }));

// Dynamic imports intentionally reload the updater singleton for each test;
// static imports would retain terminal installer state across test sessions.
beforeEach(() => {
  vi.resetModules();
  check.mockReset();
});

afterEach(() => {
  cleanup();
  vi.useRealTimers();
});

describe("session updater", () => {
  it("keeps one check and its state across Settings remounts", async () => {
    let resolveCheck!: (update: Update | null) => void;
    check.mockReturnValue(new Promise<Update | null>((resolve) => { resolveCheck = resolve; }));
    const { UpdateSection } = await import("./UpdateSection");
    const updater = await import("./updater");
    const first = render(<UpdateSection />);
    fireEvent.click(screen.getByRole("button", { name: "Check for updates" }));
    const job = updater.checkAndInstall();
    expect(updater.checkAndInstall()).toBe(job);
    await waitFor(() => expect(check).toHaveBeenCalledTimes(1));

    first.unmount();
    render(<UpdateSection />);
    expect(screen.getByRole("button", { name: "Checking…" })).toBeDisabled();
    await act(async () => {
      resolveCheck(null);
      await job;
    });
    expect(screen.getByText("Up to date")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Check for updates" })).toBeEnabled();
    expect(check).toHaveBeenCalledTimes(1);
  });

  it("keeps download progress and cannot install twice after remount or completion", async () => {
    let emit!: (event: DownloadEvent) => void;
    let finish!: () => void;
    const downloadAndInstall = vi.fn((onEvent: (event: DownloadEvent) => void) => {
      emit = onEvent;
      return new Promise<void>((resolve) => { finish = resolve; });
    });
    check.mockResolvedValue({ version: "0.1.7", body: "Fixes", downloadAndInstall });
    const { UpdateSection } = await import("./UpdateSection");
    const updater = await import("./updater");
    const first = render(<UpdateSection />);
    let job!: Promise<UpdateStatus>;
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "Check for updates" }));
      job = updater.checkAndInstall();
    });
    await waitFor(() => expect(downloadAndInstall).toHaveBeenCalledTimes(1));
    act(() => {
      emit({ event: "Started", data: { contentLength: 100 } });
      emit({ event: "Progress", data: { chunkLength: 51 } });
    });
    first.unmount();
    const second = render(<UpdateSection />);
    expect(screen.getByRole("progressbar")).toHaveAttribute("aria-valuenow", "51");
    expect(screen.getByRole("button", { name: "Downloading…" })).toBeDisabled();
    expect(updater.checkAndInstall()).toBe(job);

    const removedSubscriber = vi.fn();
    const unsubscribe = updater.subscribeToUpdates(removedSubscriber);
    unsubscribe();
    unsubscribe();
    second.unmount();
    await act(async () => {
      emit({ event: "Finished" });
      finish();
      await job;
    });
    render(<UpdateSection />);
    expect(screen.getByRole("button", { name: "Installing…" })).toBeDisabled();
    expect(updater.checkAndInstall()).toBe(job);
    expect(check).toHaveBeenCalledTimes(1);
    expect(downloadAndInstall).toHaveBeenCalledTimes(1);
    expect(removedSubscriber).not.toHaveBeenCalled();
  });

  it("preserves errors across remount and permits retry after failure", async () => {
    check.mockRejectedValueOnce(new Error("Endpoint unavailable")).mockResolvedValueOnce(null);
    const { UpdateSection } = await import("./UpdateSection");
    const first = render(<UpdateSection />);
    fireEvent.click(screen.getByRole("button", { name: "Check for updates" }));
    expect(await screen.findByText("Endpoint unavailable")).toBeInTheDocument();
    first.unmount();
    render(<UpdateSection />);
    expect(screen.getByText("Endpoint unavailable")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Check for updates" }));
    expect(await screen.findByText("Up to date")).toBeInTheDocument();
    expect(check).toHaveBeenCalledTimes(2);
  });
});

describe("startup update notice", () => {
  it("schedules a check through StrictMode effect replay without installing", async () => {
    vi.useFakeTimers();
    const downloadAndInstall = vi.fn();
    check.mockResolvedValue({ version: "0.1.7", downloadAndInstall });
    const { UpdateNotice } = await import("./UpdateNotice");
    const openSettings = vi.fn();
    render(<StrictMode><UpdateNotice onOpenSettings={openSettings} /></StrictMode>);
    await act(async () => { await vi.advanceTimersByTimeAsync(5999); });
    expect(check).not.toHaveBeenCalled();
    await act(async () => { await vi.advanceTimersByTimeAsync(1); });
    expect(check).toHaveBeenCalledTimes(1);
    expect(downloadAndInstall).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button", { name: "Update" }));
    expect(openSettings).toHaveBeenCalledTimes(1);
  });

  it("cancels the scheduled probe when the notice unmounts", async () => {
    vi.useFakeTimers();
    check.mockResolvedValue(null);
    const { UpdateNotice } = await import("./UpdateNotice");
    const notice = render(<StrictMode><UpdateNotice onOpenSettings={() => {}} /></StrictMode>);
    notice.unmount();
    await act(async () => { await vi.advanceTimersByTimeAsync(6000); });
    expect(check).not.toHaveBeenCalled();
  });
});
