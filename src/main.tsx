import React from "react";
import ReactDOM from "react-dom/client";
import "./styles/tokens.css";
import App from "./App";

// Suppress the webview's native (Edge) right-click menu app-wide, but keep it on
// editable fields so text copy/paste still works.
document.addEventListener("contextmenu", (e) => {
  const target = e.target as HTMLElement | null;
  const editable =
    target?.closest("input, textarea, [contenteditable='true']") != null;
  if (!editable) e.preventDefault();
});

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>,
);
