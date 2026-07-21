import type { ModLoader } from "../instances/types";

/** When set, Browse installs directly into this instance. */
export interface InstanceInstallTarget {
  instanceId: string;
  instanceName: string;
  minecraftVersion: string;
  loader: ModLoader;
}
