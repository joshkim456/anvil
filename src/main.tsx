import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import { api } from "./lib/api";
import { applyTheme } from "./lib/theme";
import "./styles.css";

// index.html set the palette from the localStorage cache before paint.
// Reconcile with backend Settings (the source of truth): correct the palette
// if it differs and install the live OS-change listener for "system" mode.
api
  .getSettings()
  .then((s) => applyTheme(s.theme))
  .catch(() => applyTheme("system"));

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>,
);
