import { useEffect, useState } from "react";
import type { ModSummary } from "./features/browse/types";
import { BrowsePage } from "./features/browse/BrowsePage";
import { HomePage } from "./features/home/HomePage";
import type { Tab as InstanceTab } from "./features/instance/InstancePage";
import type { InstanceInstallTarget } from "./features/navigation/types";
import { SettingsPage } from "./features/settings/SettingsPage";
import type { InstanceSummary } from "./features/instances/types";
import { AccountBar } from "./features/play/AccountBar";
import { LaunchOverlay } from "./features/play/LaunchOverlay";
import { InstallOverlay } from "./features/install/InstallOverlay";
import { usePlayStore } from "./features/play/store";
import styles from "./App.module.css";

type AppView = "home" | "browse" | "settings";

function App() {
  const [view, setView] = useState<AppView>("home");
  const initPlay = usePlayStore((s) => s.init);

  useEffect(() => {
    void initPlay();
  }, [initPlay]);
  const [installTarget, setInstallTarget] = useState<InstanceInstallTarget | null>(null);
  const [browseMod, setBrowseMod] = useState<ModSummary | null>(null);
  const [selectedInstanceId, setSelectedInstanceId] = useState<string | null>(null);
  // Lifted here (not HomePage) since HomePage itself unmounts on every trip
  // to Browse — this is the only component that survives that navigation,
  // so it's the only place "stay on Content when I come back" can live.
  const [instanceTab, setInstanceTab] = useState<InstanceTab>("overview");

  function openBrowseForInstance(instance: InstanceSummary) {
    setBrowseMod(null);
    setInstallTarget({
      instanceId: instance.id,
      instanceName: instance.name,
      minecraftVersion: instance.minecraftVersion,
      loader: instance.loader,
    });
    setView("browse");
  }

  function openModInBrowse(summary: ModSummary) {
    setInstallTarget(null);
    setBrowseMod(summary);
    setView("browse");
  }

  function clearBrowseContext() {
    setInstallTarget(null);
    setBrowseMod(null);
  }

  function goHome(clearTarget = true) {
    if (clearTarget) {
      clearBrowseContext();
    }
    setView("home");
  }

  return (
    <div className={styles.shell}>
      <header className={styles.topbar}>
        <div className={styles.brand}>
          <svg
            className={styles.brandMark}
            viewBox="0 0 16 16"
            aria-hidden
            focusable="false"
          >
            {/* Waypoint marker: diamond with a lit core */}
            <path d="M8 0.8 15.2 8 8 15.2 0.8 8Z" fill="none" stroke="currentColor" strokeWidth="1.6" />
            <path d="M8 4.8 11.2 8 8 11.2 4.8 8Z" fill="currentColor" />
          </svg>
          <span className={styles.brandName}>Waybound</span>
        </div>
        <nav className={styles.nav} aria-label="Main">
          <button
            type="button"
            className={`${styles.navItem} ${view === "home" ? styles.navItemActive : ""}`}
            aria-current={view === "home" ? "page" : undefined}
            onClick={() => goHome(true)}
          >
            My Instances
          </button>
          <button
            type="button"
            className={`${styles.navItem} ${view === "browse" ? styles.navItemActive : ""}`}
            aria-current={view === "browse" ? "page" : undefined}
            onClick={() => {
              clearBrowseContext();
              setView("browse");
            }}
          >
            Browse
          </button>
          <button
            type="button"
            className={`${styles.navItem} ${view === "settings" ? styles.navItemActive : ""}`}
            aria-current={view === "settings" ? "page" : undefined}
            onClick={() => {
              clearBrowseContext();
              setView("settings");
            }}
          >
            Settings
          </button>
        </nav>
        <div className={styles.topbarRight}>
          <AccountBar />
        </div>
      </header>

      <main className={styles.main}>
        {view === "home" && (
          <HomePage
            onAddMods={openBrowseForInstance}
            onOpenMod={openModInBrowse}
            onOpenSettings={() => setView("settings")}
            reopenInstanceId={installTarget?.instanceId ?? null}
            onReopenConsumed={() => setInstallTarget(null)}
            selectedId={selectedInstanceId}
            onSelectId={setSelectedInstanceId}
            instanceTab={instanceTab}
            onInstanceTabChange={setInstanceTab}
          />
        )}
        {view === "browse" && (
          <BrowsePage
            installTarget={installTarget}
            initialMod={browseMod}
            onInitialModConsumed={() => setBrowseMod(null)}
            onReturnToInstance={() => goHome(false)}
            onOpenSettings={() => setView("settings")}
          />
        )}
        {view === "settings" && <SettingsPage />}
      </main>

      <div className={styles.overlayStack}>
        <LaunchOverlay />
        <InstallOverlay />
      </div>
    </div>
  );
}

export default App;
