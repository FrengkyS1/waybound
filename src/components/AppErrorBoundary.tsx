import { Component, type ReactNode } from "react";
import styles from "../features/home/HomePage.module.css";

export class AppErrorBoundary extends Component<{ children: ReactNode }, { failed: boolean }> {
  state = { failed: false };

  static getDerivedStateFromError() {
    return { failed: true };
  }

  render() {
    if (!this.state.failed) return this.props.children;
    return (
      <main className={styles.page} role="alert">
        <h1>Waybound could not display this screen</h1>
        <p>Your instances and saved settings have not been removed. Reload the interface to recover. A running game will keep running.</p>
        <p>Unsaved edits will be lost. If this repeats, restart Waybound and report which screen you opened.</p>
        <button className={styles.createBtn} type="button" onClick={() => window.location.reload()}>Reload Waybound</button>
      </main>
    );
  }
}
