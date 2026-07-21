import { useEffect, useState } from "react";
import { fetchGlobalMcOptions, saveGlobalMcOptions } from "./api";
import { useTimedMessage } from "../../hooks/useTimedMessage";
import { McOptionsForm, Toggle } from "./McOptionsForm";
import { normalizeMcOptions } from "./mcOptionsDefaults";
import type { McOptions } from "./types";
import styles from "./SettingsForm.module.css";

export function GlobalGameSettingsSection() {
  const [options, setOptions] = useState<McOptions | null>(null);
  const [applyToNew, setApplyToNew] = useState(true);
  const [configured, setConfigured] = useState(false);
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const { message, showMessage, clearMessage } = useTimedMessage();

  useEffect(() => {
    void fetchGlobalMcOptions()
      .then((status) => {
        setOptions(normalizeMcOptions(status.options));
        setApplyToNew(status.applyToNewInstances);
        setConfigured(status.configured);
      })
      .catch((err) =>
        setError(err instanceof Error ? err.message : String(err)),
      )
      .finally(() => setLoading(false));
  }, []);

  function patch(partial: Partial<McOptions>) {
    setOptions((prev) => (prev ? { ...prev, ...partial } : prev));
  }

  async function handleSave() {
    if (!options) return;
    setSaving(true);
    setError(null);
    clearMessage();
    try {
      const status = await saveGlobalMcOptions(options, applyToNew);
      setConfigured(status.configured);
      showMessage("Global game settings saved.");
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setSaving(false);
    }
  }

  const disabled = !options?.customize || saving;

  return (
    <section
      className={styles.globalSection}
      aria-labelledby="global-mc-heading"
    >
      <header className={styles.globalHeader}>
        <div>
          <h2 id="global-mc-heading">Global game settings</h2>
          <p className={styles.globalHint}>
            Default Minecraft options for new instances — video, controls, key
            bindings, sound, and accessibility.
          </p>
        </div>
        <span className={configured ? styles.badgeOk : styles.badgePending}>
          {loading ? "Loading…" : configured ? "Saved" : "Using defaults"}
        </span>
      </header>

      {error && <p className={styles.errorInline}>{error}</p>}
      {message && <p className={styles.messageInline}>{message}</p>}

      {options && (
        <div className={styles.globalBody}>
          <Toggle
            label="Apply to new instances automatically"
            checked={applyToNew}
            onChange={setApplyToNew}
            disabled={!options.customize || saving}
          />

          <McOptionsForm
            options={options}
            disabled={disabled}
            onChange={patch}
          />

          <div className={styles.globalActions}>
            <button
              type="button"
              className={styles.saveBtn}
              disabled={!options.customize || saving}
              onClick={() => void handleSave()}
            >
              {saving ? "Saving…" : "Save"}
            </button>
          </div>
        </div>
      )}
    </section>
  );
}
