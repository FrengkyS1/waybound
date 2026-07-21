import { useState } from "react";

import styles from "./AccountBar.module.css";

interface PlayerHeadProps {
  uuid: string;
  initial: string;
  size?: number;
}

/**
 * The player's in-game character head (Crafatar), with the initial as a
 * fallback if the avatar can't be fetched (offline, unmigrated, etc.).
 */
export function PlayerHead({ uuid, initial, size = 24 }: PlayerHeadProps) {
  const [failed, setFailed] = useState(false);
  const px = size * 2; // request 2x for crisp rendering

  if (failed || !uuid) {
    return (
      <span
        className={styles.avatar}
        style={{ width: size, height: size }}
        aria-hidden
      >
        {initial}
      </span>
    );
  }

  return (
    <img
      className={styles.avatarImg}
      style={{ width: size, height: size }}
      src={`https://crafatar.com/avatars/${uuid}?size=${px}&overlay`}
      alt=""
      aria-hidden
      onError={() => setFailed(true)}
    />
  );
}
