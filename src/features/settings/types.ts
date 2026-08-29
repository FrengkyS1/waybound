import type { ModLoader } from "../instances/types";

export type GraphicsMode = "fast" | "fancy" | "fabulous";
export type CloudsMode = "off" | "fast" | "fancy";
export type ParticleMode = "minimal" | "decreased" | "all";
export type NarratorMode = "off" | "all" | "chat" | "system";

export interface McOptions {
  customize: boolean;
  fullscreen: boolean;
  viewBobbing: boolean;
  guiScale: number;
  gamma: number;
  renderDistance: number;
  simulationDistance: number;
  fov: number;
  entityShadows: boolean;
  vsync: boolean;
  maxFps: number;
  graphicsMode: GraphicsMode;
  clouds: CloudsMode;
  particles: ParticleMode;
  mipmapLevels: number;
  entityDistanceScaling: number;
  biomeBlendRadius: number;
  ao: boolean;
  /** 0-100 ints; options.txt stores them as 0.0-1.0 fractions. */
  screenEffectScale: number;
  fovEffectScale: number;
  darknessEffectScale: number;
  autoJump: boolean;
  /** Maps to `invertYMouse`. */
  invertMouse: boolean;
  invertXMouse: boolean;
  mouseSensitivity: number;
  rawMouseInput: boolean;
  discreteMouseScroll: boolean;
  toggleSprint: boolean;
  toggleCrouch: boolean;
  showSubtitles: boolean;
  reducedDebugInfo: boolean;
  highContrast: boolean;
  directionalAudio: boolean;
  hideLightningFlashes: boolean;
  forceUnicodeFont: boolean;
  damageTiltStrength: number;
  narrator: NarratorMode;
  language: string;
  masterVolume: number;
  musicVolume: number;
  jukeboxVolume: number;
  weatherVolume: number;
  blocksVolume: number;
  hostileVolume: number;
  neutralVolume: number;
  playerVolume: number;
  ambientVolume: number;
  voiceVolume: number;
  uiVolume: number;
  keyBindings: Record<string, string>;
}

export interface VersionPrefill {
  versionId: string;
  minecraftVersion: string;
  loader: ModLoader;
}
