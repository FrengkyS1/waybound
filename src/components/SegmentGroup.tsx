import styles from "./SegmentGroup.module.css";

export interface SegmentOption<T extends string> {
  value: T;
  label: string;
}

interface SegmentGroupProps<T extends string> {
  label: string;
  value: T;
  options: SegmentOption<T>[];
  onChange: (value: T) => void;
  compact?: boolean;
}

export function SegmentGroup<T extends string>({
  label,
  value,
  options,
  onChange,
  compact = false,
}: SegmentGroupProps<T>) {
  return (
    <div className={styles.group} role="group" aria-label={label}>
      <span className={styles.label}>{label}</span>
      <div className={`${styles.track} ${compact ? styles.trackCompact : ""}`}>
        {options.map((option) => {
          const active = option.value === value;
          return (
            <button
              key={option.value}
              type="button"
              className={`${styles.option} ${active ? styles.optionActive : ""}`}
              aria-pressed={active}
              onClick={() => onChange(option.value)}
            >
              {option.label}
            </button>
          );
        })}
      </div>
    </div>
  );
}
