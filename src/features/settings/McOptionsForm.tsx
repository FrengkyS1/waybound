import { useEffect, useState } from "react";

import type { McOptions } from "./types";
import { KEY_BINDING_LABELS } from "./mcOptionsDefaults";
import {
  codeFromKeyboardEvent,
  codeFromMouseButton,
  keyLabel,
  UNBOUND,
} from "./mcKeyBindings";
import styles from "./SettingsForm.module.css";

export function Toggle({
  label,
  checked,
  onChange,
  disabled,
}: {
  label: string;
  checked: boolean;
  onChange: (value: boolean) => void;
  disabled?: boolean;
}) {
  return (
    <label className={styles.toggleRow}>
      <span>{label}</span>
      <button
        type="button"
        role="switch"
        aria-checked={checked}
        className={`${styles.toggle} ${checked ? styles.toggleOn : ""}`}
        disabled={disabled}
        onClick={() => onChange(!checked)}
      >
        <span className={styles.toggleThumb} />
      </button>
    </label>
  );
}

export function SliderField({
  label,
  value,
  min,
  max,
  onChange,
  disabled,
  display,
}: {
  label: string;
  value: number;
  min: number;
  max: number;
  onChange: (value: number) => void;
  disabled?: boolean;
  display?: string;
}) {
  return (
    <label className={styles.sliderRow}>
      <div className={styles.sliderHead}>
        <span>{label}</span>
        <span className={styles.sliderValue}>{display ?? value}</span>
      </div>
      <input
        type="range"
        min={min}
        max={max}
        value={value}
        disabled={disabled}
        onChange={(e) => onChange(Number(e.target.value))}
      />
    </label>
  );
}

interface McOptionsFormProps {
  options: McOptions;
  disabled: boolean;
  onChange: (partial: Partial<McOptions>) => void;
  showCustomizeToggle?: boolean;
}

export function McOptionsForm({
  options,
  disabled,
  onChange,
  showCustomizeToggle = true,
}: McOptionsFormProps) {
  function patchKeyBinding(key: string, value: string) {
    onChange({
      keyBindings: {
        ...options.keyBindings,
        [key]: value,
      },
    });
  }

  const bindingKeys = Object.keys(KEY_BINDING_LABELS);

  return (
    <>
      {showCustomizeToggle && (
        <Toggle
          label="Customize game settings"
          checked={options.customize}
          onChange={(customize) => onChange({ customize })}
        />
      )}

      <section className={styles.section}>
        <h3 className={styles.sectionTitle}>Video</h3>
        <Toggle
          label="Full-screen"
          checked={options.fullscreen}
          onChange={(v) => onChange({ fullscreen: v })}
          disabled={disabled}
        />
        <Toggle
          label="View bobbing"
          checked={options.viewBobbing}
          onChange={(v) => onChange({ viewBobbing: v })}
          disabled={disabled}
        />
        <Toggle
          label="Entity shadows"
          checked={options.entityShadows}
          onChange={(v) => onChange({ entityShadows: v })}
          disabled={disabled}
        />
        <Toggle
          label="VSync"
          checked={options.vsync}
          onChange={(v) => onChange({ vsync: v })}
          disabled={disabled}
        />
        <SliderField
          label="GUI scale"
          value={options.guiScale}
          min={0}
          max={4}
          onChange={(v) => onChange({ guiScale: v })}
          disabled={disabled}
          display={options.guiScale === 0 ? "Auto" : String(options.guiScale)}
        />
        <SliderField
          label="Brightness"
          value={options.gamma}
          min={0}
          max={100}
          onChange={(v) => onChange({ gamma: v })}
          disabled={disabled}
        />
        <SliderField
          label="Render distance"
          value={options.renderDistance}
          min={2}
          max={32}
          onChange={(v) => onChange({ renderDistance: v })}
          disabled={disabled}
        />
        <SliderField
          label="Simulation distance"
          value={options.simulationDistance}
          min={5}
          max={32}
          onChange={(v) => onChange({ simulationDistance: v })}
          disabled={disabled}
        />
        <SliderField
          label="FOV"
          value={options.fov}
          min={30}
          max={110}
          onChange={(v) => onChange({ fov: v })}
          disabled={disabled}
        />
        <SliderField
          label="Max FPS"
          value={options.maxFps}
          min={10}
          max={260}
          onChange={(v) => onChange({ maxFps: v })}
          disabled={disabled}
          display={options.maxFps >= 260 ? "Unlimited" : String(options.maxFps)}
        />
        <SliderField
          label="Mipmap levels"
          value={options.mipmapLevels}
          min={0}
          max={4}
          onChange={(v) => onChange({ mipmapLevels: v })}
          disabled={disabled}
        />
        <SliderField
          label="Entity distance"
          value={options.entityDistanceScaling}
          min={50}
          max={500}
          onChange={(v) => onChange({ entityDistanceScaling: v })}
          disabled={disabled}
          display={`${options.entityDistanceScaling}%`}
        />
        <SliderField
          label="Biome blend radius"
          value={options.biomeBlendRadius}
          min={0}
          max={7}
          onChange={(v) => onChange({ biomeBlendRadius: v })}
          disabled={disabled}
        />
        <label className={styles.selectRow}>
          <span>Graphics</span>
          <select
            value={options.graphicsMode}
            disabled={disabled}
            onChange={(e) =>
              onChange({
                graphicsMode: e.target.value as McOptions["graphicsMode"],
              })
            }
          >
            <option value="fast">Fast</option>
            <option value="fancy">Fancy</option>
            <option value="fabulous">Fabulous</option>
          </select>
        </label>
        <label className={styles.selectRow}>
          <span>Clouds</span>
          <select
            value={options.clouds}
            disabled={disabled}
            onChange={(e) =>
              onChange({ clouds: e.target.value as McOptions["clouds"] })
            }
          >
            <option value="off">Off</option>
            <option value="fast">Fast</option>
            <option value="fancy">Fancy</option>
          </select>
        </label>
        <label className={styles.selectRow}>
          <span>Particles</span>
          <select
            value={options.particles}
            disabled={disabled}
            onChange={(e) =>
              onChange({ particles: e.target.value as McOptions["particles"] })
            }
          >
            <option value="all">All</option>
            <option value="decreased">Decreased</option>
            <option value="minimal">Minimal</option>
          </select>
        </label>
      </section>

      <section className={styles.section}>
        <h3 className={styles.sectionTitle}>Controls</h3>
        <Toggle
          label="Auto-jump"
          checked={options.autoJump}
          onChange={(v) => onChange({ autoJump: v })}
          disabled={disabled}
        />
        <Toggle
          label="Invert mouse"
          checked={options.invertMouse}
          onChange={(v) => onChange({ invertMouse: v })}
          disabled={disabled}
        />
        <Toggle
          label="Raw mouse input"
          checked={options.rawMouseInput}
          onChange={(v) => onChange({ rawMouseInput: v })}
          disabled={disabled}
        />
        <Toggle
          label="Discrete mouse scroll"
          checked={options.discreteMouseScroll}
          onChange={(v) => onChange({ discreteMouseScroll: v })}
          disabled={disabled}
        />
        <Toggle
          label="Toggle sprint"
          checked={options.toggleSprint}
          onChange={(v) => onChange({ toggleSprint: v })}
          disabled={disabled}
        />
        <Toggle
          label="Toggle crouch"
          checked={options.toggleCrouch}
          onChange={(v) => onChange({ toggleCrouch: v })}
          disabled={disabled}
        />
        <SliderField
          label="Mouse sensitivity"
          value={options.mouseSensitivity}
          min={0}
          max={100}
          onChange={(v) => onChange({ mouseSensitivity: v })}
          disabled={disabled}
        />
      </section>

      <section className={styles.section}>
        <h3 className={styles.sectionTitle}>Key bindings</h3>
        <p className={styles.keyHint}>
          Click a binding, then press a key or mouse button. Press Escape to
          unbind.
        </p>
        {bindingKeys.map((key) => (
          <KeyBindRow
            key={key}
            label={KEY_BINDING_LABELS[key] ?? key}
            code={options.keyBindings[key] ?? ""}
            disabled={disabled}
            onBind={(value) => patchKeyBinding(key, value)}
          />
        ))}
      </section>

      <section className={styles.section}>
        <h3 className={styles.sectionTitle}>Sound</h3>
        <SliderField
          label="Master volume"
          value={options.masterVolume}
          min={0}
          max={100}
          onChange={(v) => onChange({ masterVolume: v })}
          disabled={disabled}
        />
        <SliderField
          label="Music"
          value={options.musicVolume}
          min={0}
          max={100}
          onChange={(v) => onChange({ musicVolume: v })}
          disabled={disabled}
        />
        <SliderField
          label="Jukebox / note blocks"
          value={options.jukeboxVolume}
          min={0}
          max={100}
          onChange={(v) => onChange({ jukeboxVolume: v })}
          disabled={disabled}
        />
        <SliderField
          label="Weather"
          value={options.weatherVolume}
          min={0}
          max={100}
          onChange={(v) => onChange({ weatherVolume: v })}
          disabled={disabled}
        />
        <SliderField
          label="Blocks"
          value={options.blocksVolume}
          min={0}
          max={100}
          onChange={(v) => onChange({ blocksVolume: v })}
          disabled={disabled}
        />
        <SliderField
          label="Hostile mobs"
          value={options.hostileVolume}
          min={0}
          max={100}
          onChange={(v) => onChange({ hostileVolume: v })}
          disabled={disabled}
        />
        <SliderField
          label="Neutral mobs"
          value={options.neutralVolume}
          min={0}
          max={100}
          onChange={(v) => onChange({ neutralVolume: v })}
          disabled={disabled}
        />
        <SliderField
          label="Players"
          value={options.playerVolume}
          min={0}
          max={100}
          onChange={(v) => onChange({ playerVolume: v })}
          disabled={disabled}
        />
        <SliderField
          label="Ambient"
          value={options.ambientVolume}
          min={0}
          max={100}
          onChange={(v) => onChange({ ambientVolume: v })}
          disabled={disabled}
        />
        <SliderField
          label="Voice / speech"
          value={options.voiceVolume}
          min={0}
          max={100}
          onChange={(v) => onChange({ voiceVolume: v })}
          disabled={disabled}
        />
      </section>

      <section className={styles.section}>
        <h3 className={styles.sectionTitle}>Accessibility</h3>
        <Toggle
          label="Show subtitles"
          checked={options.showSubtitles}
          onChange={(v) => onChange({ showSubtitles: v })}
          disabled={disabled}
        />
        <Toggle
          label="Reduced debug info"
          checked={options.reducedDebugInfo}
          onChange={(v) => onChange({ reducedDebugInfo: v })}
          disabled={disabled}
        />
        <label className={styles.selectRow}>
          <span>Narrator</span>
          <select
            value={options.narrator}
            disabled={disabled}
            onChange={(e) =>
              onChange({ narrator: e.target.value as McOptions["narrator"] })
            }
          >
            <option value="off">Off</option>
            <option value="all">All</option>
            <option value="chat">Chat</option>
            <option value="system">System</option>
          </select>
        </label>
        <label className={styles.selectRow}>
          <span>Language</span>
          <input
            className={styles.textInput}
            value={options.language}
            disabled={disabled}
            onChange={(e) => onChange({ language: e.target.value })}
          />
        </label>
      </section>
    </>
  );
}

function KeyBindRow({
  label,
  code,
  disabled,
  onBind,
}: {
  label: string;
  code: string;
  disabled?: boolean;
  onBind: (code: string) => void;
}) {
  const [listening, setListening] = useState(false);

  useEffect(() => {
    if (!listening) return;

    function onKeyDown(e: KeyboardEvent) {
      e.preventDefault();
      if (e.key === "Escape") {
        onBind(UNBOUND);
      } else {
        const mapped = codeFromKeyboardEvent(e);
        if (mapped) onBind(mapped);
      }
      setListening(false);
    }
    function onMouseDown(e: MouseEvent) {
      e.preventDefault();
      onBind(codeFromMouseButton(e.button));
      setListening(false);
    }

    // Delay mouse capture a tick so the click that started listening doesn't
    // immediately bind the left mouse button.
    const timer = setTimeout(() => {
      window.addEventListener("mousedown", onMouseDown);
    }, 0);
    window.addEventListener("keydown", onKeyDown);
    return () => {
      clearTimeout(timer);
      window.removeEventListener("keydown", onKeyDown);
      window.removeEventListener("mousedown", onMouseDown);
    };
  }, [listening, onBind]);

  return (
    <div className={styles.keyRow}>
      <span>{label}</span>
      <button
        type="button"
        className={`${styles.keyBindBtn} ${listening ? styles.keyBindListening : ""}`}
        disabled={disabled}
        onClick={() => setListening((v) => !v)}
      >
        {listening ? "Press a key…" : keyLabel(code)}
      </button>
    </div>
  );
}
