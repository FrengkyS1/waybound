import { useEffect, useState } from "react";

import {
  getLaunchSettings,
  saveLaunchSettings,
  type LaunchSettings,
} from "./api";
import { usePlayStore } from "./store";
import { PlayerHead } from "./PlayerHead";
import { SignInDialog } from "./SignInDialog";
import { useTimedMessage } from "../../hooks/useTimedMessage";
import styles from "./LaunchSettingsSection.module.css";

// Flat sensible default for the global setting; modpack installs auto-scale
// past this based on mod count (see recommended_memory_mb on the Rust side).
const RECOMMENDED_MEMORY_MB = 4096;
// Matches the backend's own clamp in config/mod.rs — that clamp runs after
// Tauri's IPC layer has already deserialized this into a Rust `u32`, so a
// value bigger than u32::MAX (e.g. 20 typed digits) fails at the IPC
// boundary itself with a raw serde error, before the backend ever gets a
// chance to clamp it. Clamping here first means that path is never reached.
const MIN_MEMORY_MB = 512;
const MAX_MEMORY_MB = 32768;

export function LaunchSettingsSection() {
  const account = usePlayStore((s) => s.account);
  const signOut = usePlayStore((s) => s.signOut);

  const [settings, setSettings] = useState<LaunchSettings | null>(null);
  const [javaChoice, setJavaChoice] = useState<string>("auto");
  const [memory, setMemory] = useState(String(RECOMMENDED_MEMORY_MB));
  const [jvmArgs, setJvmArgs] = useState("");
  const [signInOpen, setSignInOpen] = useState(false);
  const [saving, setSaving] = useState(false);
  const { message, showMessage, clearMessage } = useTimedMessage();
  const [error, setError] = useState<string | null>(null);

  async function refresh() {
    const launch = await getLaunchSettings();
    setSettings(launch);
    setJavaChoice(launch.javaPath ?? "auto");
    setMemory(String(launch.maxMemoryMb));
    setJvmArgs(launch.jvmArgs ?? "");
  }

  useEffect(() => {
    void refresh().catch((err) =>
      setError(err instanceof Error ? err.message : String(err)),
    );
  }, []);

  async function handleSave() {
    setSaving(true);
    setError(null);
    clearMessage();
    try {
      await saveLaunchSettings(
        javaChoice === "auto" ? null : javaChoice,
        Math.min(Math.max(Number(memory) || RECOMMENDED_MEMORY_MB, MIN_MEMORY_MB), MAX_MEMORY_MB),
        jvmArgs.trim() || null,
      );
      await refresh();
      showMessage("Launch settings saved.");
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setSaving(false);
    }
  }

  return (
    <section className={styles.section} aria-labelledby="launch-heading">
      <div className={styles.sectionHead}>
        <h2 id="launch-heading">Account &amp; Launch</h2>
        <span className={account ? styles.statusOk : styles.statusPending}>
          {account ? `Signed in · ${account.username}` : "Not signed in"}
        </span>
      </div>

      <p className={styles.help}>
        Sign in with your Microsoft account to launch Minecraft. Waybound
        downloads the game files the first time you play an instance.
      </p>

      <div className={styles.accountRow}>
        {account ? (
          <>
            <div className={styles.account}>
              <PlayerHead
                uuid={account.uuid}
                initial={account.username.charAt(0).toUpperCase()}
                size={36}
              />
              <div>
                <span className={styles.accountName}>{account.username}</span>
                <span className={styles.accountUuid}>{account.uuid}</span>
              </div>
            </div>
            <button
              type="button"
              className={styles.secondary}
              onClick={() => void signOut()}
            >
              Sign out
            </button>
          </>
        ) : (
          <button
            type="button"
            className={styles.primary}
            onClick={() => setSignInOpen(true)}
          >
            Sign in with Microsoft
          </button>
        )}
      </div>

      <div className={styles.grid}>
        <label className={styles.field} htmlFor="java-select">
          <span className={styles.label}>Java runtime</span>
          <select
            id="java-select"
            className={styles.input}
            value={javaChoice}
            onChange={(e) => setJavaChoice(e.target.value)}
          >
            <option value="auto">Automatic (match each version)</option>
            {settings?.detected.map((rt) => (
              <option key={rt.path} value={rt.path}>
                Java {rt.majorVersion} — {rt.path}
              </option>
            ))}
          </select>
          <span className={styles.fieldHint}>
            {settings && settings.detected.length === 0
              ? "No Java found. Install Adoptium Temurin (17 for 1.17+, 21 for 1.20.5+)."
              : `${settings?.detected.length ?? 0} runtime(s) detected.`}
          </span>
        </label>

        <label className={styles.field} htmlFor="memory-input">
          <span className={styles.label}>Max memory (MB)</span>
          <div className={styles.inputRow}>
            <input
              id="memory-input"
              type="text"
              inputMode="numeric"
              className={styles.input}
              value={memory}
              onChange={(e) => setMemory(e.target.value.replace(/[^0-9]/g, ""))}
            />
            <button
              type="button"
              className={styles.secondary}
              onClick={() => setMemory(String(RECOMMENDED_MEMORY_MB))}
            >
              Use recommended
            </button>
          </div>
          <span className={styles.fieldHint}>
            {RECOMMENDED_MEMORY_MB} MB suits most modpacks. Heavier packs (80+
            mods) get more automatically when installed.
          </span>
        </label>
      </div>

      <label className={styles.field} htmlFor="jvm-args">
        <span className={styles.label}>Custom JVM arguments (global)</span>
        <textarea
          id="jvm-args"
          className={styles.textarea}
          value={jvmArgs}
          onChange={(e) => setJvmArgs(e.target.value)}
          placeholder="-XX:+UseG1GC -Dfile.encoding=UTF-8"
          spellCheck={false}
          rows={2}
        />
        <span className={styles.fieldHint}>
          Applied to every instance unless the instance overrides them. Later
          flags win over Waybound's defaults.
        </span>
      </label>

      <div className={styles.actions}>
        <button
          type="button"
          className={styles.primary}
          disabled={saving}
          onClick={() => void handleSave()}
        >
          {saving ? "Saving…" : "Save launch settings"}
        </button>
      </div>

      {message && <p className={styles.message}>{message}</p>}
      {error && <p className={styles.error}>{error}</p>}

      {signInOpen && <SignInDialog onClose={() => setSignInOpen(false)} />}
    </section>
  );
}
