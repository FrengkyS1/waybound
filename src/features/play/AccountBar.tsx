import { useEffect, useRef, useState } from "react";

import { usePlayStore } from "./store";
import { PlayerHead } from "./PlayerHead";
import { SignInDialog } from "./SignInDialog";
import { useEscapeKey } from "../../hooks/useEscapeKey";
import styles from "./AccountBar.module.css";

export function AccountBar() {
  const account = usePlayStore((s) => s.account);
  const accountLoaded = usePlayStore((s) => s.accountLoaded);
  const signOut = usePlayStore((s) => s.signOut);
  const [dialogOpen, setDialogOpen] = useState(false);
  const [menuOpen, setMenuOpen] = useState(false);
  const menuRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!menuOpen) return;
    function onClick(e: MouseEvent) {
      if (menuRef.current && !menuRef.current.contains(e.target as Node))
        setMenuOpen(false);
    }
    document.addEventListener("mousedown", onClick);
    return () => document.removeEventListener("mousedown", onClick);
  }, [menuOpen]);

  useEscapeKey(() => setMenuOpen(false), menuOpen);

  if (!accountLoaded) {
    return <div className={styles.placeholder} aria-hidden />;
  }

  if (!account) {
    return (
      <>
        <button
          type="button"
          className={styles.signIn}
          onClick={() => setDialogOpen(true)}
        >
          Sign in
        </button>
        {dialogOpen && <SignInDialog onClose={() => setDialogOpen(false)} />}
      </>
    );
  }

  const initial = account.username.charAt(0).toUpperCase() || "?";

  return (
    <div className={styles.wrap} ref={menuRef}>
      <button
        type="button"
        className={styles.chip}
        onClick={() => setMenuOpen((v) => !v)}
        aria-haspopup="menu"
        aria-expanded={menuOpen}
      >
        <PlayerHead uuid={account.uuid} initial={initial} />
        <span className={styles.username}>{account.username}</span>
      </button>
      {menuOpen && (
        <div className={styles.menu} role="menu">
          <span className={styles.menuLabel}>
            Signed in as {account.username}
          </span>
          <button
            type="button"
            className={styles.menuItem}
            role="menuitem"
            onClick={() => {
              setMenuOpen(false);
              void signOut();
            }}
          >
            Sign out
          </button>
        </div>
      )}
    </div>
  );
}
