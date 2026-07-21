import type { ModSummary } from "../browse/types";
import type { InstalledMod } from "./types";

const EMPTY_SUMMARY = {
  description: "",
  author: "",
  iconUrl: null as string | null,
  downloads: 0,
  loaders: [] as ModSummary["loaders"],
  updatedAt: "",
};

export function modSummaryFromInstalled(mod: InstalledMod): ModSummary | null {
  if (mod.modUid.startsWith("file:")) {
    return null;
  }

  if (mod.modUid.startsWith("mod:cf:")) {
    const id = Number.parseInt(mod.modUid.slice("mod:cf:".length), 10);
    if (Number.isNaN(id)) return null;
    return {
      ...EMPTY_SUMMARY,
      uid: mod.modUid,
      slug: String(id),
      name: mod.modName,
      projectType: "mod",
      sources: ["curseforge"],
      curseforgeId: id,
    };
  }

  if (mod.modUid.startsWith("mod:slug:")) {
    const slug = mod.modUid.slice("mod:slug:".length);
    return {
      ...EMPTY_SUMMARY,
      uid: mod.modUid,
      slug,
      name: mod.modName,
      projectType: "mod",
      sources: [mod.source],
      modrinthId: mod.source === "modrinth" ? slug : undefined,
      curseforgeId: mod.source === "curseforge" ? undefined : undefined,
    };
  }

  if (mod.modUid.startsWith("mod:")) {
    const id = mod.modUid.slice("mod:".length);
    const isModrinth = mod.source === "modrinth";
    return {
      ...EMPTY_SUMMARY,
      uid: mod.modUid,
      slug: id,
      name: mod.modName,
      projectType: "mod",
      sources: [mod.source],
      modrinthId: isModrinth ? id : undefined,
    };
  }

  return null;
}

export function canOpenInstalledMod(mod: InstalledMod): boolean {
  return modSummaryFromInstalled(mod) !== null;
}
