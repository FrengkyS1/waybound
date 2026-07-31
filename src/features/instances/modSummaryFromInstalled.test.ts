import { describe, expect, it } from "vitest";

import { canOpenInstalledMod, modSummaryFromInstalled } from "./modSummaryFromInstalled";
import type { InstalledMod } from "./types";

function installed(overrides: Partial<InstalledMod> = {}): InstalledMod {
  return {
    id: 1,
    instanceId: "inst-1",
    modUid: "mod:slug:sodium",
    modName: "Sodium",
    source: "modrinth",
    fileName: "sodium-0.5.3.jar",
    installedAt: 1_700_000_000,
    ...overrides,
  };
}

describe("modSummaryFromInstalled", () => {
  it("returns null for a locally-added file, which has no project page", () => {
    expect(modSummaryFromInstalled(installed({ modUid: "file:sodium-0.5.3.jar" }))).toBeNull();
  });

  it("checks the file: prefix before anything else", () => {
    // A path that happens to contain a project-shaped substring is still a
    // local file, not something openable in Browse.
    expect(modSummaryFromInstalled(installed({ modUid: "file:mod:cf:394468" }))).toBeNull();
  });

  it("returns null for a uid in no recognised namespace", () => {
    for (const modUid of ["", "sodium", "MOD:slug:sodium", "curseforge:394468", "mods:cf:1"]) {
      expect(modSummaryFromInstalled(installed({ modUid })), modUid).toBeNull();
    }
  });

  describe("curseforge uids", () => {
    it("parses the numeric project id and marks the summary as curseforge", () => {
      const summary = modSummaryFromInstalled(
        installed({ modUid: "mod:cf:394468", modName: "Sodium", source: "curseforge" }),
      );

      expect(summary).toEqual({
        uid: "mod:cf:394468",
        slug: "394468",
        name: "Sodium",
        projectType: "mod",
        sources: ["curseforge"],
        curseforgeId: 394468,
        description: "",
        author: "",
        iconUrl: null,
        downloads: 0,
        loaders: [],
        updatedAt: "",
      });
    });

    it("trusts the uid prefix over a mislabelled source field", () => {
      const summary = modSummaryFromInstalled(
        installed({ modUid: "mod:cf:394468", source: "modrinth" }),
      );
      expect(summary?.sources).toEqual(["curseforge"]);
      expect(summary?.curseforgeId).toBe(394468);
      expect(summary?.modrinthId).toBeUndefined();
    });

    it("returns null when the id after the prefix is not a number", () => {
      expect(modSummaryFromInstalled(installed({ modUid: "mod:cf:" }))).toBeNull();
      expect(modSummaryFromInstalled(installed({ modUid: "mod:cf:abc" }))).toBeNull();
      expect(modSummaryFromInstalled(installed({ modUid: "mod:cf:-" }))).toBeNull();
    });

    it("accepts a leading-numeric id, since parseInt stops at the first junk", () => {
      // Documenting the lenient parse rather than pretending it validates.
      const summary = modSummaryFromInstalled(installed({ modUid: "mod:cf:12abc" }));
      expect(summary?.curseforgeId).toBe(12);
      expect(summary?.slug).toBe("12");
      // The uid is kept verbatim so it still matches the installed record.
      expect(summary?.uid).toBe("mod:cf:12abc");
    });
  });

  describe("slug uids", () => {
    it("carries the slug into modrinthId for a modrinth install", () => {
      const summary = modSummaryFromInstalled(
        installed({ modUid: "mod:slug:sodium", modName: "Sodium", source: "modrinth" }),
      );

      expect(summary?.slug).toBe("sodium");
      expect(summary?.uid).toBe("mod:slug:sodium");
      expect(summary?.name).toBe("Sodium");
      expect(summary?.sources).toEqual(["modrinth"]);
      expect(summary?.modrinthId).toBe("sodium");
      expect(summary?.curseforgeId).toBeUndefined();
    });

    it("leaves both ids unset for a curseforge slug install", () => {
      // A curseforge slug can't be turned into the numeric id its API needs,
      // so the summary carries the source but no id — the detail lookup has
      // to fall back to the slug.
      const summary = modSummaryFromInstalled(
        installed({ modUid: "mod:slug:jei", source: "curseforge" }),
      );

      expect(summary?.slug).toBe("jei");
      expect(summary?.sources).toEqual(["curseforge"]);
      expect(summary?.modrinthId).toBeUndefined();
      expect(summary?.curseforgeId).toBeUndefined();
    });

    it("keeps a slug containing further colons intact", () => {
      const summary = modSummaryFromInstalled(installed({ modUid: "mod:slug:a:b" }));
      expect(summary?.slug).toBe("a:b");
      expect(summary?.modrinthId).toBe("a:b");
    });

    it("yields an empty slug rather than null for a bare prefix", () => {
      const summary = modSummaryFromInstalled(installed({ modUid: "mod:slug:" }));
      expect(summary).not.toBeNull();
      expect(summary?.slug).toBe("");
    });
  });

  describe("bare mod: uids", () => {
    it("treats the remainder as a modrinth project id", () => {
      const summary = modSummaryFromInstalled(
        installed({ modUid: "mod:AANobbMI", modName: "Sodium", source: "modrinth" }),
      );

      expect(summary?.uid).toBe("mod:AANobbMI");
      expect(summary?.slug).toBe("AANobbMI");
      expect(summary?.modrinthId).toBe("AANobbMI");
      expect(summary?.sources).toEqual(["modrinth"]);
      expect(summary?.curseforgeId).toBeUndefined();
    });

    it("sets no id at all when the source is curseforge", () => {
      const summary = modSummaryFromInstalled(
        installed({ modUid: "mod:AANobbMI", source: "curseforge" }),
      );

      expect(summary?.sources).toEqual(["curseforge"]);
      expect(summary?.modrinthId).toBeUndefined();
      expect(summary?.curseforgeId).toBeUndefined();
    });

    it("still fills the empty-summary fields", () => {
      const summary = modSummaryFromInstalled(installed({ modUid: "mod:x" }));
      expect(summary?.description).toBe("");
      expect(summary?.author).toBe("");
      expect(summary?.iconUrl).toBeNull();
      expect(summary?.downloads).toBe(0);
      expect(summary?.loaders).toEqual([]);
      expect(summary?.updatedAt).toBe("");
      expect(summary?.projectType).toBe("mod");
    });

    it("shares one empty loaders array across every summary it builds", () => {
      // The empty-summary template is spread, so the array reference is
      // shared. Nothing mutates it today; pinned so that if a caller ever
      // pushes into `summary.loaders` the breakage shows up here rather than
      // as every installed mod suddenly claiming the same loaders.
      const a = modSummaryFromInstalled(installed({ modUid: "mod:x" }));
      const b = modSummaryFromInstalled(installed({ modUid: "mod:y" }));
      expect(a?.loaders).toBe(b?.loaders);
      expect(a?.loaders).toEqual([]);
    });
  });

  it("always reports projectType mod, even for a resource pack's record", () => {
    // The installed-mods table doesn't track project type, so everything it
    // reconstructs is a "mod" as far as the Browse page is concerned.
    const summary = modSummaryFromInstalled(
      installed({ modUid: "mod:slug:faithful", modName: "Faithful" }),
    );
    expect(summary?.projectType).toBe("mod");
  });
});

describe("canOpenInstalledMod", () => {
  it("mirrors whether a summary could be reconstructed", () => {
    expect(canOpenInstalledMod(installed({ modUid: "mod:cf:394468" }))).toBe(true);
    expect(canOpenInstalledMod(installed({ modUid: "mod:slug:sodium" }))).toBe(true);
    expect(canOpenInstalledMod(installed({ modUid: "mod:AANobbMI" }))).toBe(true);

    expect(canOpenInstalledMod(installed({ modUid: "file:whatever.jar" }))).toBe(false);
    expect(canOpenInstalledMod(installed({ modUid: "mod:cf:nope" }))).toBe(false);
    expect(canOpenInstalledMod(installed({ modUid: "" }))).toBe(false);
  });
});
