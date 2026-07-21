import { useEffect, useState } from "react";

import { useTimedMessage } from "../../hooks/useTimedMessage";
import {
  getInstanceLaunchConfig,
  getLaunchSettings,
  setInstanceLaunchConfig,
  type JavaRuntime,
} from "../play/api";
import styles from "./InstancePage.module.css";

interface LaunchOverridesProps {
  instanceId: string;
}

/**
 * Per-instance launch overrides. Any field left on "Use global" falls back to
 * the global launch settings; a value here overrides just this instance.
 */
export function LaunchOverrides({ instanceId }: LaunchOverridesProps) {
  const [detected, setDetected] = useState<JavaRuntime[]>([]);
  const [javaChoice, setJavaChoice] = useState("global");
  const [memory, setMemory] = useState("");
  const [jvmArgs, setJvmArgs] = useState("");
  const [saving, setSaving] = useState(false);
  const { message, showMessage, clearMessage } = useTimedMessage();
  const [error, setError] = useState<string | null>(null);

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

  async function handleSave() {
    setSaving(true);
    setError(null);
    clearMessage();
    try {
      await setInstanceLaunchConfig(instanceId, {
        javaPath: javaChoice === "global" ? null : javaChoice,
        maxMemoryMb: memory.trim() ? Number(memory) : null,
        jvmArgs: jvmArgs.trim() || null,
      });
      showMessage("Instance launch overrides saved.");
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setSaving(false);
    }
  }

  return (
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
  );
}
