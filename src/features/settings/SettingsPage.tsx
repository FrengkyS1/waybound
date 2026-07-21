import { useEffect, useState } from "react";

import {
  clearCurseForgeApiKey,
  fetchCurseForgeStatus,
  importCurseForgeApiKeyFromEnvFile,
  saveCurseForgeApiKey,
  testDockerCurseForgeEnvKey,
  testSavedCurseForgeApiKey,
  type CurseForgeKeySource,
} from "./api";

import styles from "./SettingsPage.module.css";
import { GlobalGameSettingsSection } from "./GlobalGameSettingsSection";
import { LaunchSettingsSection } from "../play/LaunchSettingsSection";

function sourceLabel(source?: CurseForgeKeySource): string | null {
  if (source === "config") return "Saved in Waybound";

  if (source === "environment") return "From CF_API_KEY environment";

  return null;
}

export function SettingsPage() {
  const [configured, setConfigured] = useState(false);

  const [keySource, setKeySource] = useState<CurseForgeKeySource | undefined>();

  const [envAvailable, setEnvAvailable] = useState(false);

  const [apiKey, setApiKey] = useState("");

  const [envFilePath, setEnvFilePath] = useState("");

  const [loading, setLoading] = useState(true);

  const [saving, setSaving] = useState(false);

  const [message, setMessage] = useState<string | null>(null);

  const [error, setError] = useState<string | null>(null);

  const [probeLog, setProbeLog] = useState<string[] | null>(null);

  async function refreshStatus() {
    const status = await fetchCurseForgeStatus();

    setConfigured(status.configured);

    setKeySource(status.source);

    setEnvAvailable(status.environmentAvailable);
    setEnvFilePath((current) => current || status.defaultDockerEnvPath || "");
  }

  useEffect(() => {
    void (async () => {
      try {
        await refreshStatus();
      } catch (err) {
        setError(err instanceof Error ? err.message : String(err));
      } finally {
        setLoading(false);
      }
    })();
  }, []);

  function clearFeedback() {
    setError(null);

    setMessage(null);

    setProbeLog(null);
  }

  async function handleSave(event: React.FormEvent) {
    event.preventDefault();

    setSaving(true);

    clearFeedback();

    try {
      await saveCurseForgeApiKey(apiKey, true);

      await refreshStatus();

      setApiKey("");

      setMessage(
        "CurseForge API key saved. Accepts raw keys or a Docker `.env` line (`CF_API_KEY=...`). Use “Test saved key” to verify.",
      );
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setSaving(false);
    }
  }

  async function handleImportEnvFile() {
    setSaving(true);

    clearFeedback();

    try {
      await importCurseForgeApiKeyFromEnvFile(envFilePath, true);

      await refreshStatus();

      setMessage("Imported CF_API_KEY from your `.env` file into Waybound.");
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setSaving(false);
    }
  }

  async function handleTestSaved() {
    setSaving(true);

    clearFeedback();

    try {
      const result = await testSavedCurseForgeApiKey();

      setProbeLog(result.log);

      if (result.ok) {
        setMessage(result.message);
      } else {
        setError(formatProbeSummary(result));
      }
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setSaving(false);
    }
  }

  async function handleTestDockerEnv() {
    setSaving(true);

    clearFeedback();

    try {
      const result = await testDockerCurseForgeEnvKey(envFilePath || undefined);

      setProbeLog(result.log);

      if (result.ok) {
        setMessage(
          `${result.message} (Docker-style key works — use Import to save it in Waybound.)`,
        );
      } else {
        setError(formatProbeSummary(result));
      }
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setSaving(false);
    }
  }

  async function handleClear() {
    setSaving(true);

    clearFeedback();

    try {
      await clearCurseForgeApiKey();

      await refreshStatus();

      setApiKey("");

      setMessage(
        "CurseForge API key removed from Waybound (environment variable unchanged).",
      );
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setSaving(false);
    }
  }

  const activeSource = sourceLabel(keySource);

  return (
    <div className={styles.page}>
      <header className={styles.header}>
        <h1>Settings</h1>

        <p className={styles.subtitle}>Account, launch, and integrations</p>
      </header>

      <div className={styles.grid}>
        <div className={styles.left}>
          <LaunchSettingsSection />

      <section className={styles.section} aria-labelledby="curseforge-heading">
        <div className={styles.sectionHead}>
          <h2 id="curseforge-heading">CurseForge API</h2>

          <span className={configured ? styles.statusOk : styles.statusPending}>
            {loading
              ? "Checking…"
              : configured
                ? "Configured"
                : "Not configured"}
          </span>
        </div>

        {activeSource && (
          <p className={styles.help}>Active key: {activeSource}</p>
        )}
        {envAvailable && (
          <p className={styles.help}>
            CF_API_KEY is also set in the process environment.
          </p>
        )}

        <p className={styles.help}>
          Get a key from{" "}
          <a
            href="https://console.curseforge.com/#/api-keys"
            target="_blank"
            rel="noreferrer"
          >
            console.curseforge.com → API Keys
          </a>
          , paste it below, and save.
        </p>

        <form
          className={styles.form}
          onSubmit={(event) => {
            void handleSave(event);
          }}
        >
          <label className={styles.field} htmlFor="cf-api-key">
            <span className={styles.label}>API key or .env line</span>
            <input
              id="cf-api-key"
              type="password"
              className={styles.input}
              value={apiKey}
              onChange={(event) => setApiKey(event.currentTarget.value)}
              placeholder="CF_API_KEY=$2a$10$… or paste raw key"
              autoComplete="off"
              spellCheck={false}
            />
          </label>

          <div className={styles.actions}>
            <button
              type="submit"
              className={styles.primary}
              disabled={saving || !apiKey.trim()}
            >
              {saving ? "Saving…" : "Save key"}
            </button>

            {configured && (
              <>
                <button
                  type="button"
                  className={styles.secondary}
                  disabled={saving}
                  onClick={() => void handleTestSaved()}
                >
                  Test saved key
                </button>

                <button
                  type="button"
                  className={styles.secondary}
                  disabled={saving}
                  onClick={() => void handleClear()}
                >
                  Remove key
                </button>
              </>
            )}
          </div>
        </form>

        <details className={styles.disclosure}>
          <summary className={styles.disclosureSummary}>
            Using a Docker .env file instead?
          </summary>

          <p className={styles.help}>
            In your Docker <code className={styles.code}>.env</code>:{" "}
            <code className={styles.code}>CF_API_KEY=$2a$10$…</code>. If Docker
            fails with forbidden, double each{" "}
            <code className={styles.code}>$</code> (
            <a
              href="https://github.com/itzg/docker-minecraft-server/discussions/2588"
              target="_blank"
              rel="noreferrer"
            >
              itzg#2588
            </a>
            ). Waybound accepts both formats.
          </p>

          <p className={styles.help}>
            Your compose template uses{" "}
            <code className={styles.code}>
              CF_API_KEY: &apos;${"{"}CF_API_KEY{"}"}&apos;
            </code>{" "}
            — paste or import from the{" "}
            <code className={styles.code}>.env</code> next to{" "}
            <code className={styles.code}>docker-compose.yml</code>, not the
            YAML itself.
          </p>

          <label className={styles.field} htmlFor="cf-env-path">
            <span className={styles.label}>
              Docker .env (docker/prominence2/.env)
            </span>
            <input
              id="cf-env-path"
              type="text"
              className={styles.input}
              value={envFilePath}
              onChange={(event) => setEnvFilePath(event.currentTarget.value)}
              placeholder="docker/prominence2/.env — auto-detected when empty"
              autoComplete="off"
              spellCheck={false}
            />
          </label>

          <div className={styles.actions}>
            <button
              type="button"
              className={styles.secondary}
              disabled={saving}
              onClick={() => void handleImportEnvFile()}
            >
              Import .env file
            </button>
            <button
              type="button"
              className={styles.secondary}
              disabled={saving}
              onClick={() => void handleTestDockerEnv()}
            >
              Test Docker .env key
            </button>
          </div>
        </details>

        {message && <p className={styles.message}>{message}</p>}

        {error && <p className={styles.error}>{error}</p>}

        {probeLog && probeLog.length > 0 && (
          <div className={styles.probeLogWrap}>
            <p className={styles.probeLogTitle}>Probe log</p>

            <pre className={styles.probeLog}>{probeLog.join("\n")}</pre>
          </div>
        )}
      </section>
        </div>

        <div className={styles.right}>
          <GlobalGameSettingsSection />
        </div>
      </div>
    </div>
  );
}

function formatProbeSummary(result: {
  message: string;

  httpStatus: number;

  keyPrefix: string;

  keyLength: number;
}): string {
  return [
    result.message,

    result.httpStatus ? `HTTP ${result.httpStatus}` : null,

    result.keyPrefix ? `Key prefix: ${result.keyPrefix}` : null,

    `Key length: ${result.keyLength}`,
  ]

    .filter(Boolean)

    .join(" · ");
}
