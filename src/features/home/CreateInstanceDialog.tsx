import { useEffect, useRef, useState } from "react";
import type { ModLoader } from "../instances/types";
import { fileToIconDataUrl } from "./imageIcon";
import { useEscapeKey } from "../../hooks/useEscapeKey";
import styles from "./CreateInstanceDialog.module.css";

const LOADERS: { value: ModLoader; label: string }[] = [
  { value: "vanilla", label: "Vanilla" },
  { value: "fabric", label: "Fabric" },
  { value: "forge", label: "Forge" },
  { value: "neoforge", label: "NeoForge" },
  { value: "quilt", label: "Quilt" },
];

interface CreateInstanceDialogProps {
  versions: string[];
  busy: boolean;
  onClose: () => void;
  onCreate: (input: {
    name: string;
    minecraftVersion: string;
    loader: ModLoader;
    icon: string | null;
  }) => void;
}

export function CreateInstanceDialog({
  versions,
  busy,
  onClose,
  onCreate,
}: CreateInstanceDialogProps) {
  const [name, setName] = useState("");
  const [mcVersion, setMcVersion] = useState(versions[0] ?? "1.21.1");
  const [loader, setLoader] = useState<ModLoader>("vanilla");
  const [icon, setIcon] = useState<string | null>(null);
  const fileRef = useRef<HTMLInputElement>(null);

  useEscapeKey(onClose, !busy);

  async function handlePickImage(e: React.ChangeEvent<HTMLInputElement>) {
    const file = e.target.files?.[0];
    e.target.value = "";
    if (!file) return;
    try {
      setIcon(await fileToIconDataUrl(file));
    } catch {
      /* ignore unreadable images */
    }
  }

  useEffect(() => {
    if (versions.length > 0 && !versions.includes(mcVersion)) {
      setMcVersion(versions[0]);
    }
  }, [versions, mcVersion]);

  const canSubmit = name.trim().length >= 2 && mcVersion && !busy;

  return (
    <div
      className={styles.backdrop}
      onClick={busy ? undefined : onClose}
      role="presentation"
    >
      <div
        className={styles.dialog}
        role="dialog"
        aria-modal="true"
        aria-labelledby="create-instance-title"
        onClick={(e) => e.stopPropagation()}
      >
        <header className={styles.header}>
          <h2 id="create-instance-title" className={styles.title}>
            New instance
          </h2>
          <p className={styles.subtitle}>
            Choose a Minecraft version and mod loader. You can install mods from
            Browse afterward.
          </p>
        </header>

        <form
          className={styles.form}
          onSubmit={(e) => {
            e.preventDefault();
            if (!canSubmit) return;
            onCreate({
              name: name.trim(),
              minecraftVersion: mcVersion,
              loader,
              icon,
            });
          }}
        >
          <div className={styles.nameRow}>
            <button
              type="button"
              className={styles.iconPicker}
              onClick={() => fileRef.current?.click()}
              aria-label="Choose instance image"
            >
              {icon ? (
                <img className={styles.iconImg} src={icon} alt="" />
              ) : (
                <span className={styles.iconPlaceholder}>
                  {name.trim().charAt(0).toUpperCase() || "+"}
                </span>
              )}
              <span className={styles.iconOverlay}>Image</span>
            </button>
            <input
              ref={fileRef}
              type="file"
              accept="image/*"
              hidden
              onChange={(e) => void handlePickImage(e)}
            />
            <label className={styles.field}>
              <span className={styles.label}>Name</span>
              <input
                className={styles.input}
                value={name}
                onChange={(e) => setName(e.target.value)}
                placeholder="Survival Fabric"
                autoFocus
                required
                minLength={2}
                maxLength={100}
              />
            </label>
          </div>

          <fieldset className={styles.fieldset}>
            <legend className={styles.label}>Minecraft version</legend>
            <div className={styles.versionList}>
              {versions.length === 0 && (
                <p className={styles.hint}>Loading versions…</p>
              )}
              {versions.map((version) => (
                <button
                  key={version}
                  type="button"
                  className={`${styles.versionBtn} ${
                    version === mcVersion ? styles.versionBtnActive : ""
                  }`}
                  aria-pressed={version === mcVersion}
                  onClick={() => setMcVersion(version)}
                >
                  {version}
                </button>
              ))}
            </div>
          </fieldset>

          <fieldset className={styles.fieldset}>
            <legend className={styles.label}>Mod loader</legend>
            <div className={styles.loaderRow}>
              {LOADERS.map((item) => (
                <button
                  key={item.value}
                  type="button"
                  className={`${styles.loaderBtn} ${
                    item.value === loader ? styles.loaderBtnActive : ""
                  }`}
                  aria-pressed={item.value === loader}
                  onClick={() => setLoader(item.value)}
                >
                  {item.label}
                </button>
              ))}
            </div>
          </fieldset>

          <footer className={styles.footer}>
            <button
              type="button"
              className={styles.cancelBtn}
              onClick={onClose}
              disabled={busy}
            >
              Cancel
            </button>
            <button
              type="submit"
              className={styles.submitBtn}
              disabled={!canSubmit}
            >
              {busy ? "Creating…" : "Create instance"}
            </button>
          </footer>
        </form>
      </div>
    </div>
  );
}
