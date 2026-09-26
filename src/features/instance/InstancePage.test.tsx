import { mockIPC } from "@tauri-apps/api/mocks";
import { fireEvent, render, screen, within } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import type { InstanceContent } from "../instances/api";
import type { InstanceSummary } from "../instances/types";

/**
 * `formatSize`, `humanizeFileName`, `formatDuration`, `formatLastPlayed` and
 * `formatDate` are all module-private, so they are driven through the output
 * they produce: the Overview tab's stat row and the Content tab's rows.
 * InstancePage pulls in `playStore` and `installStore`, both of which wire
 * event listeners at module scope, so it must be imported after the IPC mock
 * exists — hence the dynamic import.
 */
type InstancePageModule = typeof import("./InstancePage");

let InstancePage: InstancePageModule["InstancePage"];

function summary(over: Partial<InstanceSummary> = {}): InstanceSummary {
  return {
    id: "inst-1",
    name: "Test Instance",
    minecraftVersion: "1.20.1",
    loader: "fabric",
    modCount: 3,
    createdAt: 1_700_000_000, // 2023-11-14 UTC
    rootPath: "C:/instances/inst-1",
    totalPlaySeconds: 0,
    lastPlayed: null,
    ...over,
  };
}

const noop = () => {};
const asyncNoop = async () => {};

async function renderPage(
  instance: InstanceSummary,
  tab: "overview" | "content" = "overview",
) {
  if (!InstancePage) ({ InstancePage } = await import("./InstancePage"));
  render(
    <InstancePage
      instance={instance}
      busy={false}
      loaderLabel="Fabric"
      onBack={noop}
      onDelete={noop}
      onDuplicate={noop}
      onAddMods={noop}
      onChangeImage={noop}
      onRename={asyncNoop}
      onLoaderVersionChange={asyncNoop}
      tab={tab}
      onTabChange={noop}
    />,
  );
}

/** The value rendered next to a stat label in the hero stat row. */
function statValue(label: string): string {
  const el = screen.getByText(label).nextElementSibling;
  return el?.textContent ?? "";
}

beforeEach(() => {
  // ContentTab lazily resolves row metadata through an IntersectionObserver,
  // which jsdom does not implement. Never intersecting is fine here: the
  // rows still render with whatever the content listing already carried.
  vi.stubGlobal(
    "IntersectionObserver",
    class {
      observe() {}
      unobserve() {}
      disconnect() {}
      takeRecords() {
        return [];
      }
    },
  );
});

describe("formatDuration (Play time stat)", () => {
  it.each([
    [0, "\u2014"],
    [45, "45s"],
    [59, "59s"],
    [60, "1m"],
    [599, "9m"],
    [3599, "59m"],
    [3600, "1h 0m"],
    [7320, "2h 2m"],
    [86_400, "24h 0m"],
  ])("renders %i seconds as %s", async (seconds, expected) => {
    await renderPage(summary({ totalPlaySeconds: seconds }));
    expect(statValue("Play time")).toBe(expected);
  });

  it("does not round seconds up into a minute", async () => {
    // Sub-minute sessions keep their second count rather than collapsing to
    // "0m", which would read as "never really played".
    await renderPage(summary({ totalPlaySeconds: 1 }));
    expect(statValue("Play time")).toBe("1s");
  });

  it("drops the seconds once there are whole minutes", async () => {
    await renderPage(summary({ totalPlaySeconds: 125 }));
    expect(statValue("Play time")).toBe("2m");
  });
});

describe("formatLastPlayed (Last played stat)", () => {
  it("reads Never for an instance that has not been launched", async () => {
    await renderPage(summary({ lastPlayed: null }));
    expect(statValue("Last played")).toBe("Never");
  });

  it("reads Never for a missing timestamp", async () => {
    await renderPage(summary({ lastPlayed: undefined }));
    expect(statValue("Last played")).toBe("Never");
  });

  it("treats a zero timestamp as never rather than as 1970", async () => {
    await renderPage(summary({ lastPlayed: 0 }));
    expect(statValue("Last played")).toBe("Never");
  });

  it("formats a real timestamp as a date", async () => {
    await renderPage(summary({ lastPlayed: 1_700_000_000 }));
    const value = statValue("Last played");
    expect(value).not.toBe("Never");
    expect(value).toMatch(/2023/);
  });
});

describe("formatDate (Created stat)", () => {
  it("reads the stored value as unix SECONDS, not milliseconds", async () => {
    // The backend stores seconds. Feeding them to `new Date()` unscaled puts
    // every instance in January 1970.
    await renderPage(summary({ createdAt: 1_700_000_000 }));

    const value = statValue("Created");
    expect(value).toMatch(/2023/);
    expect(value).not.toMatch(/1970/);
  });

  it("renders a short month name rather than a raw number soup", async () => {
    await renderPage(summary({ createdAt: 1_700_000_000 }));
    expect(statValue("Created")).toBe(
      new Date(1_700_000_000_000).toLocaleDateString(undefined, {
        year: "numeric",
        month: "short",
        day: "numeric",
      }),
    );
  });

  it("distinguishes two instances created years apart", async () => {
    await renderPage(summary({ createdAt: 1_000_000_000 }));
    const older = statValue("Created");
    screen.getByText("Created"); // sanity: still the same stat row
    expect(older).toMatch(/2001/);
  });
});

describe("Content tab file rows", () => {
  const entry = (over: Partial<InstanceContent["mods"][number]>) => ({
    fileName: "file.jar",
    enabled: true,
    sizeBytes: 0,
    metaResolved: true,
    hasConfig: false,
    addedByYou: false,
    ...over,
  });

  async function renderContent(content: Partial<InstanceContent>) {
    mockIPC((cmd) => {
      if (cmd === "list_instance_content")
        return { mods: [], resourcePacks: [], shaderPacks: [], ...content };
      return undefined;
    });
    await renderPage(summary(), "content");
  }

  it("humanizes a file name only when the jar declares no name of its own", async () => {
    await renderContent({
      mods: [
        entry({ fileName: "sodium-fabric_0.5.3.jar", name: "Sodium" }),
        entry({ fileName: "fabric-api-0.92.0.jar" }),
      ],
    });

    // A declared name wins outright.
    expect(await screen.findByText("Sodium")).toBeInTheDocument();
    // Otherwise: drop the extension, then dashes/underscores become spaces.
    expect(screen.getByText("fabric api 0.92.0")).toBeInTheDocument();
  });

  it("strips Minecraft colour codes some shader packs bake into the zip name", async () => {
    await renderContent({
      shaderPacks: [entry({ fileName: "\u00a7aComplementary\u00a7r-Shaders.zip" })],
    });

    expect(await screen.findByText("Complementary Shaders")).toBeInTheDocument();
  });

  it("collapses a run of separators into a single space", async () => {
    await renderContent({ mods: [entry({ fileName: "some__weird--name.JAR" })] });

    // The extension match is case-insensitive.
    expect(await screen.findByText("some weird name")).toBeInTheDocument();
  });

  it("leaves a name alone when there is nothing to clean up", async () => {
    await renderContent({ mods: [entry({ fileName: "lithium.jar" })] });
    expect(await screen.findByText("lithium")).toBeInTheDocument();
  });

  it("only strips a trailing archive extension, not one mid-name", async () => {
    await renderContent({ mods: [entry({ fileName: "not.jar.but.a-mod.jar" })] });
    expect(await screen.findByText("not.jar.but.a mod")).toBeInTheDocument();
  });

  it("scales byte counts to B / KB / MB on the row's tooltip", async () => {
    await renderContent({
      mods: [
        entry({ fileName: "tiny.jar", sizeBytes: 0 }),
        entry({ fileName: "small.jar", sizeBytes: 500 }),
        entry({ fileName: "edge-b.jar", sizeBytes: 1023 }),
        entry({ fileName: "exact-kb.jar", sizeBytes: 1024 }),
        entry({ fileName: "rounded-kb.jar", sizeBytes: 1536 }),
        entry({ fileName: "edge-kb.jar", sizeBytes: 1_048_575 }),
        entry({ fileName: "exact-mb.jar", sizeBytes: 1_048_576 }),
        entry({ fileName: "big.jar", sizeBytes: 2_500_000 }),
      ],
    });

    const sizeOf = (fileName: string) => screen.getByText(fileName).getAttribute("title");

    await screen.findByText("tiny.jar");
    expect(sizeOf("tiny.jar")).toBe("0 B");
    expect(sizeOf("small.jar")).toBe("500 B");
    expect(sizeOf("edge-b.jar")).toBe("1023 B");
    expect(sizeOf("exact-kb.jar")).toBe("1 KB");
    // KB is rounded, not truncated.
    expect(sizeOf("rounded-kb.jar")).toBe("2 KB");
    expect(sizeOf("edge-kb.jar")).toBe("1024 KB");
    // MB keeps one decimal, so a 1 MB file doesn't read as a bare "1 MB".
    expect(sizeOf("exact-mb.jar")).toBe("1.0 MB");
    expect(sizeOf("big.jar")).toBe("2.4 MB");
  });

  it("badges user-added mods and filters by origin", async () => {    await renderContent({
      mods: [
        entry({ fileName: "mine.jar", name: "Mine", addedByYou: true }),
        entry({ fileName: "pack.jar", name: "Pack", addedByYou: false }),
      ],
    });

    await screen.findByText("Mine");
    expect(screen.getAllByText("Added by you")).toHaveLength(1);

    const addedGroup = screen.getByRole("group", { name: "Added" });
    // Narrow to just user-added rows.
    fireEvent.click(within(addedGroup).getByRole("button", { name: /by you/i }));
    expect(screen.getByText("mine.jar")).toBeInTheDocument();
    expect(screen.queryByText("pack.jar")).not.toBeInTheDocument();

    // And back to everything.
    fireEvent.click(within(addedGroup).getByRole("button", { name: /^all/i }));
    expect(screen.getByText("pack.jar")).toBeInTheDocument();
  });

  it("searches resolved display names, not just filenames", async () => {
    await renderContent({
      mods: [
        entry({ fileName: "ftb-ranks-neoforge-2101.1.3.jar", name: "FTB Ranks" }),
        entry({ fileName: "unrelated-1.0.0.jar", name: "Unrelated" }),
      ],
    });

    await screen.findByText("FTB Ranks");
    // "ftb r" has a space the filename never contains — only the
    // display-name match finds it.
    fireEvent.change(screen.getByRole("searchbox"), { target: { value: "ftb r" } });
    expect(screen.getByText("FTB Ranks")).toBeInTheDocument();
    expect(screen.queryByText("Unrelated")).not.toBeInTheDocument();
  });
});
