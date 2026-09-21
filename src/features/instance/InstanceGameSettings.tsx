import { useEffect, useState } from "react";
import { fetchInstanceOptions, saveInstanceOptions } from "../settings/api";
import { McOptionsForm } from "../settings/McOptionsForm";
import { normalizeMcOptions } from "../settings/mcOptionsDefaults";
import type { McOptions } from "../settings/types";
import { usePlayStore } from "../play/store";
import styles from "./InstancePage.module.css";

export function InstanceGameSettings({ instanceId, busy }: { instanceId: string; busy: boolean }) {
  const [options, setOptions] = useState<McOptions | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [message, setMessage] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const [attempt, setAttempt] = useState(0);
  const launch = usePlayStore((state) => state.launches[instanceId]);
  const running = launch?.phase === "running" || launch?.phase === "preparing";
  useEffect(() => {
    let active = true;
    setError(null);
    void fetchInstanceOptions(instanceId).then((value) => {
      if (active) setOptions(normalizeMcOptions(value));
    }).catch((cause) => { if (active) setError(String(cause)); });
    return () => { active = false; };
  }, [instanceId, attempt]);

  async function save() {
    if (!options || saving || busy || running) return;
    setSaving(true);
    setError(null);
    setMessage(null);
    try {
      await saveInstanceOptions(instanceId, options);
      setMessage("Game settings saved for this instance.");
    } catch (cause) {
      setError(String(cause));
    } finally {
      setSaving(false);
    }
  }

  return (
    <section className={styles.settingsCard} aria-label="Instance game settings">
      <h2 className={styles.cardTitle}>Game settings (this instance)</h2>
      <p className={styles.note}>Video, sound, controls and accessibility for this instance only. Close Minecraft before editing so the game cannot overwrite your changes.</p>
      {running && <p className={styles.note}>Stop this instance before saving game settings.</p>}
      {error && <p className={styles.error} role="alert">{error} {!options && <button type="button" onClick={() => setAttempt((value) => value + 1)}>Retry</button>}</p>}
      {message && <p className={styles.message} role="status">{message}</p>}
      {!options && !error && <p role="status">Loading game settings…</p>}
      {options && <>
        <fieldset disabled={saving || busy || running} style={{ border: 0, padding: 0, margin: 0 }}>
          <McOptionsForm options={options} disabled={!options.customize || saving || busy || running}
            onChange={(patch) => setOptions((previous) => previous ? { ...previous, ...patch } : previous)} />
        </fieldset>
        <button type="button" className={styles.primaryBtn} disabled={saving || busy || running} onClick={() => void save()}>{saving ? "Saving…" : "Save game settings"}</button>
      </>}
    </section>
  );
}
