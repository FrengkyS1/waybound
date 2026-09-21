import { useEffect, useState } from "react";

import { ConfirmDialog } from "../../components/ConfirmDialog";
import { useTimedMessage } from "../../hooks/useTimedMessage";
import { getLatestLoaderVersion, getLoaderVersionInfo } from "../instances/api";
import type { InstanceSummary } from "../instances/types";
import {
  getInstanceLaunchConfig,
  getLaunchSettings,
  setInstanceLaunchConfig,
  type JavaRuntime,
} from "../play/api";
import styles from "./InstancePage.module.css";

// Matches the backend's own clamp bounds for the global setting — a typed
// value bigger than u32::MAX fails at the Tauri IPC boundary itself (before
// any backend clamp runs), so this needs to happen before the value is ever
// sent.
const MIN_MEMORY_MB = 512;
const MAX_MEMORY_MB = 32768;

interface LaunchOverridesProps {
  instance: InstanceSummary;
  /** Pins (or, with `null`, un-pins) this instance's loader build. */
  onLoaderVersionChange: (loaderVersion: string | null) => Promise<void>;
}

const LOADER_HAS_LATEST_BUILD = new Set(["fabric", "forge", "neoforge", "quilt"]);

/**
 * Per-instance launch overrides. Any field left on "Use global" falls back to
 * the global launch settings; a value here overrides just this instance.
 */
export function LaunchOverrides({ instance, onLoaderVersionChange }: LaunchOverridesProps) {
  const instanceId = instance.id;
  const [detected, setDetected] = useState<JavaRuntime[]>([]);
  const [javaChoice, setJavaChoice] = useState("global");
  const [memory, setMemory] = useState("");
  const [jvmArgs, setJvmArgs] = useState("");
  const [saving, setSaving] = useState(false);
  const { message, showMessage, clearMessage } = useTimedMessage();
  const [error, setError] = useState<string | null>(null);

  const [latestLoaderVersion, setLatestLoaderVersion] = useState<string | null>(null);
  const [recommendedLoaderVersion, setRecommendedLoaderVersion] = useState<string | null>(null);
  const [loaderInfoStale, setLoaderInfoStale] = useState(false);
  const [checkingLoader, setCheckingLoader] = useState(false);
  const [applyingLoader, setApplyingLoader] = useState(false);
  const [loaderError, setLoaderError] = useState<string | null>(null);
  const [confirmApplyOpen, setConfirmApplyOpen] = useState(false);

  useEffect(() => {
    void (async () => {
      try {
        const [global, config] = await Promise.all([
          getLaunchSettings(),
          getInstanceLaunchConfig(instanceId),
        ]);
        setDetected(global.detected);
        setJavaChoice(config.javaPath ?? "global");
        setMemory(config.maxMemoryMb ? String(config.maxMemoryMb) : "");
        setJvmArgs(config.jvmArgs ?? "");
      } catch (err) {
        setError(err instanceof Error ? err.message : String(err));
      }
    })();
  }, [instanceId]);

  // A previous instance's "here's a newer build" result has no bearing on
  // whatever instance is showing now — drop it rather than let it linger
  // across a navigation.
  useEffect(() => {
    setLatestLoaderVersion(null);
    setRecommendedLoaderVersion(null);
    setLoaderInfoStale(false);
    setLoaderError(null);
  }, [instanceId]);

  const supportsLoaderUpdate = LOADER_HAS_LATEST_BUILD.has(instance.loader);

  async function handleCheckForLoaderUpdate() {
    setCheckingLoader(true);
    setLoaderError(null);
    try {
      // The cached index covers every loader (including Fabric/Quilt, which
      // the legacy lookup below doesn't know) and reports recommended
      // separately; Forge keeps its conservative recommended-first offer.
      const info = await getLoaderVersionInfo(instance.loader, instance.minecraftVersion);
      const offer =
        instance.loader === "forge" ? (info.recommended ?? info.latest) : info.latest;
      setLatestLoaderVersion(offer);
      setRecommendedLoaderVersion(
        info.recommended && info.recommended !== offer ? info.recommended : null,
      );
      setLoaderInfoStale(info.fromCache);
    } catch (err) {
      // Fall back to the legacy direct lookup rather than failing outright
      // (e.g. a cold cache with no network for the index but a reachable
      // promotions endpoint is near-impossible, but cheap to cover).
      try {
        setLatestLoaderVersion(await getLatestLoaderVersion(instanceId));
        setRecommendedLoaderVersion(null);
        setLoaderInfoStale(false);
      } catch (fallbackErr) {
        setLoaderError(
          fallbackErr instanceof Error ? fallbackErr.message : String(fallbackErr),
        );
      }
    } finally {
      setCheckingLoader(false);
    }
  }

  async function handleApplyLoaderVersion() {
    if (!latestLoaderVersion) return;
    setApplyingLoader(true);
    setLoaderError(null);
    try {
      await onLoaderVersionChange(latestLoaderVersion);
      showMessage(
        `Loader version set to ${latestLoaderVersion}. It'll download the next time you launch.`,
      );
      setLatestLoaderVersion(null);
    } catch (err) {
      setLoaderError(err instanceof Error ? err.message : String(err));
    } finally {
      setApplyingLoader(false);
      setConfirmApplyOpen(false);
    }
  }

  async function handleSave() {
    setSaving(true);
    setError(null);
    clearMessage();
    try {
      const clampedMemory = memory.trim()
        ? Math.min(Math.max(Number(memory), MIN_MEMORY_MB), MAX_MEMORY_MB)
        : null;
      await setInstanceLaunchConfig(instanceId, {
        javaPath: javaChoice === "global" ? null : javaChoice,
        maxMemoryMb: clampedMemory,
        jvmArgs: jvmArgs.trim() || null,
      });
      // Without this, typing an out-of-range value (e.g. "0") kept showing
      // exactly what was typed even though the backend clamped it to 512 —
      // the field never reflected what was actually saved and would keep
      // looking wrong on every future visit to this tab.
      setMemory(clampedMemory ? String(clampedMemory) : "");
      showMessage("Instance launch overrides saved.");
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setSaving(false);
    }
  }

  return (
    <>
    <section className={styles.settingsCard}>
      <div className={styles.settingsHead}>
        <h2 className={styles.cardTitle}>
          Java &amp; performance (this instance)
        </h2>
        <button
          type="button"
          className={styles.primaryBtn}
          onClick={() => void handleSave()}
          disabled={saving}
        >
          {saving ? "Saving…" : "Save overrides"}
        </button>
      </div>
      <p className={styles.note}>
        Overrides the global launch settings for this instance only. Leave a
        field on “Use global” or blank to inherit.
      </p>

      <div className={styles.overrideGrid}>
        <label className={styles.field}>
          <span className={styles.fieldLabel}>Java runtime</span>
          <select
            className={styles.input}
            value={javaChoice}
            onChange={(e) => setJavaChoice(e.target.value)}
          >
            <option value="global">Use global / automatic</option>
            {detected.map((rt) => (
              <option key={rt.path} value={rt.path}>
                Java {rt.majorVersion} — {rt.path}
              </option>
            ))}
          </select>
        </label>

        <label className={styles.field}>
          <span className={styles.fieldLabel}>Max memory (MB)</span>
          <input
            type="text"
            inputMode="numeric"
            className={styles.input}
            value={memory}
            placeholder="Global"
            onChange={(e) => setMemory(e.target.value.replace(/[^0-9]/g, ""))}
          />
        </label>
      </div>

      <label className={styles.field}>
        <span className={styles.fieldLabel}>Custom JVM arguments</span>
        <textarea
          className={styles.textarea}
          value={jvmArgs}
          placeholder="Leave blank to use global args"
          spellCheck={false}
          rows={2}
          onChange={(e) => setJvmArgs(e.target.value)}
        />
      </label>

      {error && <p className={styles.error}>{error}</p>}
      {message && <p className={styles.message}>{message}</p>}
    </section>

    {supportsLoaderUpdate && (
      <section className={styles.settingsCard}>
        <div className={styles.settingsHead}>
          <h2 className={styles.cardTitle}>Loader version</h2>
          <button
            type="button"
            className={styles.ghostBtn}
            onClick={() => void handleCheckForLoaderUpdate()}
            disabled={checkingLoader}
          >
            {checkingLoader ? "Checking…" : "Check for update"}
          </button>
        </div>
        <p className={styles.note}>
          {instance.loaderVersion
            ? `Loader version: ${instance.loaderVersion}`
            : "Loader version: automatic (uses whatever build is currently recommended at launch)."}
          {latestLoaderVersion && latestLoaderVersion !== instance.loaderVersion && (
            <> - a newer build, {latestLoaderVersion}, is available.</>
          )}
          {latestLoaderVersion && latestLoaderVersion === instance.loaderVersion && (
            <> You're on the latest recommended build.</>
          )}
          {recommendedLoaderVersion && (
            <> Recommended build: {recommendedLoaderVersion}.</>
          )}
          {loaderInfoStale && latestLoaderVersion && <> (from cache — offline)</>}
        </p>
        {latestLoaderVersion && latestLoaderVersion !== instance.loaderVersion && (
          <button
            type="button"
            className={styles.primaryBtn}
            onClick={() => setConfirmApplyOpen(true)}
            disabled={applyingLoader}
          >
            Update to {latestLoaderVersion}
          </button>
        )}
        {loaderError && <p className={styles.error}>{loaderError}</p>}
      </section>
    )}

    {confirmApplyOpen && latestLoaderVersion && (
      <ConfirmDialog
        title="Update loader version?"
        message={`This sets ${instance.name}'s loader to ${latestLoaderVersion}. It downloads automatically the next time you launch.`}
        confirmLabel={applyingLoader ? "Updating…" : "Update"}
        onConfirm={() => void handleApplyLoaderVersion()}
        onCancel={() => setConfirmApplyOpen(false)}
      />
    )}
    </>
  );
}
