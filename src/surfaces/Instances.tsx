import { useCallback, useEffect, useRef, useState } from "react";
import { openUrl } from "@tauri-apps/plugin-opener";
import {
  api,
  AuthAccount,
  AuthEvent,
  DeviceCode,
  Instance,
  LaunchEvent,
  on,
} from "../lib/api";

export default function Instances() {
  const [instances, setInstances] = useState<Instance[]>([]);
  const [account, setAccount] = useState<AuthAccount | null>(null);

  const [device, setDevice] = useState<DeviceCode | null>(null);
  const [authBusy, setAuthBusy] = useState(false);
  const [authError, setAuthError] = useState<string | null>(null);

  const [logLines, setLogLines] = useState<string[]>([]);
  const [launchStatus, setLaunchStatus] = useState<string | null>(null);
  const logRef = useRef<HTMLDivElement | null>(null);

  const [importPath, setImportPath] = useState("");
  const [importBusy, setImportBusy] = useState(false);
  const [importError, setImportError] = useState<string | null>(null);

  const refreshInstances = useCallback(() => {
    api.listInstances().then(setInstances).catch(() => {});
  }, []);

  const refreshAccount = useCallback(() => {
    api
      .authStatus()
      .then(setAccount)
      .catch(() => setAccount(null));
  }, []);

  useEffect(() => {
    refreshInstances();
    refreshAccount();
  }, [refreshInstances, refreshAccount]);

  // Microsoft device-code sign-in result.
  useEffect(() => {
    const p = on<AuthEvent>("auth-event", (e) => {
      if ("error" in e) {
        setAuthError(e.error);
        setAuthBusy(false);
        return;
      }
      if (e.status === "signed_in") {
        setDevice(null);
        setAuthBusy(false);
        setAuthError(null);
        refreshAccount();
      }
    });
    return () => {
      p.then((un) => un());
    };
  }, [refreshAccount]);

  // Launch progress + logs.
  useEffect(() => {
    const p = on<LaunchEvent>("launch-event", (e) => {
      switch (e.kind) {
        case "status":
          setLaunchStatus(e["0"]);
          break;
        case "progress":
          setLaunchStatus(`${e.what}: ${e.done}/${e.total}`);
          break;
        case "log":
          setLogLines((l) => [...l, e["0"]]);
          break;
        case "exited":
          setLaunchStatus(`Exited (code ${e["0"]})`);
          break;
        case "error":
          setLaunchStatus(null);
          setLogLines((l) => [...l, `error: ${e["0"]}`]);
          break;
      }
    });
    return () => {
      p.then((un) => un());
    };
  }, []);

  useEffect(() => {
    if (logRef.current) logRef.current.scrollTop = logRef.current.scrollHeight;
  }, [logLines]);

  async function signIn() {
    setAuthBusy(true);
    setAuthError(null);
    try {
      const d = await api.authStart();
      setDevice(d);
    } catch (e) {
      setAuthError(String(e));
      setAuthBusy(false);
    }
  }

  async function signOut() {
    try {
      await api.authSignout();
    } catch {
      /* ignore */
    }
    setAccount(null);
  }

  async function launch(id: string) {
    setLogLines([]);
    setLaunchStatus("Starting…");
    try {
      await api.launchInstance(id);
    } catch (e) {
      setLaunchStatus(null);
      setLogLines((l) => [...l, `error: ${String(e)}`]);
    }
  }

  async function doImport() {
    const path = importPath.trim();
    if (!path) return;
    setImportBusy(true);
    setImportError(null);
    try {
      await api.importMrpack(path);
      setImportPath("");
      refreshInstances();
    } catch (e) {
      setImportError(String(e));
    } finally {
      setImportBusy(false);
    }
  }

  return (
    <>
      <div className="page-head">
        <h2>Instances</h2>
      </div>

      <div className="account-bar">
        {account ? (
          <>
            <span className="account-chip">
              <span className="dot" aria-hidden /> {account.username}
            </span>
            <button className="btn ghost" onClick={signOut}>
              Sign out
            </button>
          </>
        ) : (
          <button className="btn" onClick={signIn} disabled={authBusy}>
            {authBusy ? "Starting…" : "Sign in with Microsoft"}
          </button>
        )}
        {authError && <span className="account-err">{authError}</span>}
      </div>

      <div className="import-row">
        <input
          placeholder="Path to .mrpack file"
          value={importPath}
          onChange={(e) => setImportPath(e.target.value)}
        />
        <button
          className="btn ghost"
          onClick={doImport}
          disabled={importBusy || !importPath.trim()}
        >
          {importBusy ? "Importing…" : "Import .mrpack"}
        </button>
      </div>
      {importError && <div className="error">{importError}</div>}

      {instances.length === 0 ? (
        <div className="placeholder">
          <strong>No instances yet</strong>
          Design one in the Curator or import a .mrpack.
        </div>
      ) : (
        <div className="mod-grid">
          {instances.map((i) => (
            <div className="mod-card" key={i.id}>
              <span className="icon-fallback">
                {i.name.slice(0, 1).toUpperCase()}
              </span>
              <div style={{ minWidth: 0, flex: 1 }}>
                <h3>{i.name}</h3>
                <div className="by">
                  {i.loader} · MC {i.mc_version} · {i.mods.length} mods
                </div>
                <div className="meta">
                  <span>
                    last played {i.last_played?.slice(0, 10) ?? "never"}
                  </span>
                </div>
                <div className="card-actions">
                  <button
                    className="btn"
                    onClick={() => launch(i.id)}
                    disabled={!account}
                  >
                    Launch
                  </button>
                  {!account && (
                    <span className="card-hint">Sign in to launch</span>
                  )}
                </div>
              </div>
            </div>
          ))}
        </div>
      )}

      {(launchStatus || logLines.length > 0) && (
        <div className="launch-log">
          <div className="launch-log-head">
            <span>Launch log</span>
            {launchStatus && (
              <span className="launch-status">{launchStatus}</span>
            )}
          </div>
          <div className="launch-log-body" ref={logRef}>
            {logLines.map((line, idx) => (
              <div key={idx}>{line}</div>
            ))}
          </div>
        </div>
      )}

      {device && (
        <div className="overlay" onClick={() => undefined}>
          <div
            className="auth-modal"
            onClick={(e) => e.stopPropagation()}
          >
            <h3>Sign in with Microsoft</h3>
            <div className="auth-code">{device.user_code}</div>
            <button
              className="btn"
              onClick={() => {
                openUrl(device.verification_uri).catch(() => {});
              }}
            >
              Open {device.verification_uri}
            </button>
            <p className="auth-wait">Waiting for sign-in.</p>
            {authError && <div className="error">{authError}</div>}
            <button
              className="btn ghost"
              onClick={() => {
                setDevice(null);
                setAuthBusy(false);
              }}
            >
              Cancel
            </button>
          </div>
        </div>
      )}
    </>
  );
}
