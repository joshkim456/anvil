import { useCallback, useEffect, useRef, useState } from "react";
import {
  api,
  AuthAccount,
  AuthEvent,
  Instance,
  KeybindReport,
  LaunchEvent,
  UpdateInfo,
  formatEditError,
  on,
} from "../lib/api";
import { Dropdown, Opt } from "../components/Dropdown";
import { eventToToken, mouseButtonToToken, tokenDisplay } from "../lib/mckeys";

const cap = (s: string) => s[0].toUpperCase() + s.slice(1);

const MC_OPTS: Opt[] = ["1.21.1", "1.21", "1.20.1", "1.19.2", "1.18.2"].map(
  (v) => ({ value: v, label: v }),
);
const LOADER_OPTS: Opt[] = [
  "vanilla",
  "fabric",
  "forge",
  "neoforge",
  "quilt",
].map((v) => ({ value: v, label: cap(v) }));

export default function Instances({
  onOpenChat,
}: {
  onOpenChat: (instanceId: string, name: string) => void;
}) {
  const [instances, setInstances] = useState<Instance[]>([]);
  const [account, setAccount] = useState<AuthAccount | null>(null);

  const [authBusy, setAuthBusy] = useState(false);
  const [authError, setAuthError] = useState<string | null>(null);
  const [logLines, setLogLines] = useState<string[]>([]);
  const [launchStatus, setLaunchStatus] = useState<string | null>(null);
  const [verifying, setVerifying] = useState<string | null>(null);
  const [verifyMsg, setVerifyMsg] = useState<Record<string, string>>({});
  const logRef = useRef<HTMLDivElement | null>(null);

  const [importPath, setImportPath] = useState("");
  const [importBusy, setImportBusy] = useState(false);
  const [importError, setImportError] = useState<string | null>(null);

  // New-instance modal.
  const [showCreate, setShowCreate] = useState(false);
  const [nName, setNName] = useState("");
  const [nMc, setNMc] = useState(MC_OPTS[0].value);
  const [nLoader, setNLoader] = useState(LOADER_OPTS[0].value);
  const [nLoaderVer, setNLoaderVer] = useState("");
  const [createBusy, setCreateBusy] = useState(false);
  const [createError, setCreateError] = useState<string | null>(null);

  // Per-card state.
  const [expanded, setExpanded] = useState<string | null>(null);
  const [confirmDelete, setConfirmDelete] = useState<Instance | null>(null);
  const [dupTarget, setDupTarget] = useState<Instance | null>(null);
  const [dupName, setDupName] = useState("");
  const [dupBusy, setDupBusy] = useState(false);
  const [dupError, setDupError] = useState<string | null>(null);

  // Keybinds modal.
  const [kbTarget, setKbTarget] = useState<Instance | null>(null);
  const [kbReport, setKbReport] = useState<KeybindReport | null>(null);
  const [kbBusy, setKbBusy] = useState(false);
  const [kbError, setKbError] = useState<string | null>(null);
  // name -> new token (only changed rows); plus which row is capturing.
  const [kbEdits, setKbEdits] = useState<Record<string, string>>({});
  const [kbCapturing, setKbCapturing] = useState<string | null>(null);

  // Check-updates modal.
  const [updTarget, setUpdTarget] = useState<Instance | null>(null);
  const [updBusy, setUpdBusy] = useState(false);
  const [updError, setUpdError] = useState<string | null>(null);
  const [updates, setUpdates] = useState<UpdateInfo[] | null>(null);
  const [updSel, setUpdSel] = useState<string[]>([]);

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

  // Microsoft sign-in result (webview flow runs in the backend).
  useEffect(() => {
    const p = on<AuthEvent>("auth-event", (e) => {
      if ("error" in e) {
        setAuthError(e.error);
        setAuthBusy(false);
        return;
      }
      if (e.status === "signed_in") {
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
          setLaunchStatus(e.data);
          break;
        case "progress":
          setLaunchStatus(
            `${e.data.what}: ${e.data.done}/${e.data.total}`,
          );
          break;
        case "log":
          setLogLines((l) => [...l, e.data]);
          break;
        case "exited":
          setLaunchStatus(`Exited (code ${e.data})`);
          break;
        case "error":
          setLaunchStatus(null);
          setLogLines((l) => [...l, `error: ${e.data}`]);
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
      // Opens the Microsoft login window; resolves on signed_in/error via
      // the auth-event listener above.
      await api.authStart();
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
    setLaunchStatus("Starting.");
    try {
      await api.launchInstance(id);
    } catch (e) {
      setLaunchStatus(null);
      setLogLines((l) => [...l, `error: ${String(e)}`]);
    }
  }

  // Tier 3: boot the pack once and report whether mods initialize cleanly,
  // so a broken pack is caught here instead of by the user playing it.
  async function verify(id: string) {
    setVerifying(id);
    setVerifyMsg((m) => ({
      ...m,
      [id]: "Booting the pack once (~1 min, no window)…",
    }));
    setLogLines([]);
    setLaunchStatus("Smoke test running.");
    try {
      const v = await api.smokeTestInstance(id);
      const msg =
        v.kind === "ok"
          ? "✓ Mods initialize cleanly"
          : v.kind === "failed"
            ? `✗ ${v.data.mod_name ? `[${v.data.mod_name}] ` : ""}${v.data.reason}`
            : `? ${v.data.reason}`;
      setVerifyMsg((m) => ({ ...m, [id]: msg }));
    } catch (e) {
      setVerifyMsg((m) => ({ ...m, [id]: `error: ${String(e)}` }));
    } finally {
      setVerifying(null);
      setLaunchStatus(null);
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

  function openCreate() {
    setNName("");
    setNMc(MC_OPTS[0].value);
    setNLoader(LOADER_OPTS[0].value);
    setNLoaderVer("");
    setCreateError(null);
    setShowCreate(true);
  }

  async function doCreate() {
    const name = nName.trim();
    if (!name) return;
    setCreateBusy(true);
    setCreateError(null);
    try {
      await api.createInstance({
        name,
        mcVersion: nMc,
        loader: nLoader,
        loaderVersion: nLoaderVer.trim(),
      });
      setShowCreate(false);
      refreshInstances();
    } catch (e) {
      setCreateError(String(e));
    } finally {
      setCreateBusy(false);
    }
  }

  function doDuplicate(i: Instance) {
    setDupTarget(i);
    setDupName(`${i.name} copy`);
    setDupError(null);
  }

  async function confirmDuplicate() {
    if (!dupTarget || !dupName.trim()) return;
    setDupBusy(true);
    setDupError(null);
    try {
      await api.duplicateInstance(dupTarget.id, dupName.trim());
      setDupTarget(null);
      refreshInstances();
    } catch (e) {
      setDupError(String(e));
    } finally {
      setDupBusy(false);
    }
  }

  async function doDelete(i: Instance) {
    try {
      await api.deleteInstance(i.id);
      setConfirmDelete(null);
      if (expanded === i.id) setExpanded(null);
      refreshInstances();
    } catch (e) {
      setImportError(String(e));
      setConfirmDelete(null);
    }
  }

  async function removeMod(instanceId: string, projectId: string) {
    setImportError(null);
    try {
      await api.removeModFromInstance(instanceId, projectId);
      refreshInstances();
    } catch (e) {
      // Structured rejection (e.g. still_required lists the mods that
      // still depend on this one); never String(e) — that prints
      // "[object Object]" and hides the path forward.
      setImportError(formatEditError(e));
    }
  }

  async function openUpdates(i: Instance) {
    setUpdTarget(i);
    setUpdates(null);
    setUpdSel([]);
    setUpdError(null);
    setUpdBusy(true);
    try {
      const list = await api.checkInstanceUpdates(i.id);
      setUpdates(list);
      setUpdSel(list.map((u) => u.project_id));
    } catch (e) {
      setUpdError(String(e));
    } finally {
      setUpdBusy(false);
    }
  }

  async function openKeybinds(i: Instance) {
    setKbTarget(i);
    setKbReport(null);
    setKbEdits({});
    setKbCapturing(null);
    setKbError(null);
    setKbBusy(true);
    try {
      setKbReport(await api.getKeybinds(i.id));
    } catch (e) {
      setKbError(String(e));
    } finally {
      setKbBusy(false);
    }
  }

  function closeKeybinds() {
    setKbTarget(null);
    setKbCapturing(null);
  }

  async function saveKeybinds() {
    if (!kbTarget) return;
    const changes = Object.entries(kbEdits).map(([name, token]) => ({
      name,
      token,
    }));
    if (changes.length === 0) return closeKeybinds();
    setKbBusy(true);
    setKbError(null);
    try {
      await api.setKeybinds(kbTarget.id, changes);
      closeKeybinds();
    } catch (e) {
      setKbError(String(e));
    } finally {
      setKbBusy(false);
    }
  }

  // While a row is capturing, the next key (or mouse button) rebinds it.
  useEffect(() => {
    if (!kbCapturing) return;
    const finish = (token: string) => {
      setKbEdits((m) => ({ ...m, [kbCapturing]: token }));
      setKbCapturing(null);
    };
    const onKey = (e: KeyboardEvent) => {
      e.preventDefault();
      if (e.key === "Escape") return setKbCapturing(null);
      const t = eventToToken(e);
      if (t) finish(t);
    };
    const onMouse = (e: MouseEvent) => {
      e.preventDefault();
      finish(mouseButtonToToken(e.button));
    };
    window.addEventListener("keydown", onKey, true);
    window.addEventListener("mousedown", onMouse, true);
    return () => {
      window.removeEventListener("keydown", onKey, true);
      window.removeEventListener("mousedown", onMouse, true);
    };
  }, [kbCapturing]);

  function toggleUpd(pid: string) {
    setUpdSel((s) =>
      s.includes(pid) ? s.filter((x) => x !== pid) : [...s, pid],
    );
  }

  async function applyUpdates() {
    if (!updTarget || updSel.length === 0) return;
    setUpdBusy(true);
    setUpdError(null);
    try {
      await api.applyInstanceUpdates(updTarget.id, updSel);
      setUpdTarget(null);
      setUpdates(null);
      refreshInstances();
    } catch (e) {
      setUpdError(String(e));
    } finally {
      setUpdBusy(false);
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
          <button className="btn ghost" onClick={signIn} disabled={authBusy}>
            {authBusy ? "Opening sign-in." : "Sign in with Microsoft"}
          </button>
        )}
        {authError && <span className="account-err">{authError}</span>}
        <span className="spacer" />
        <button className="btn" onClick={openCreate}>
          New instance
        </button>
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
          {importBusy ? "Importing." : "Import .mrpack"}
        </button>
      </div>
      {importError && (
        <div className="error" style={{ whiteSpace: "pre-line" }}>
          {importError}
        </div>
      )}

      {instances.length === 0 ? (
        <div className="placeholder">
          <strong>No instances yet</strong>
          Create one above, design one in the Curator, or import a .mrpack.
        </div>
      ) : (
        <div className="mod-grid">
          {instances.map((i) => (
            <div className="mod-card inst-card" key={i.id}>
              <div className="inst-top">
                <span className="icon-fallback">
                  {i.name.slice(0, 1).toUpperCase()}
                </span>
                <div style={{ minWidth: 0, flex: 1 }}>
                  <div className="inst-title">{i.name}</div>
                  <div className="inst-spec">
                    <b>{cap(i.loader)}</b>
                    <b>MC {i.mc_version}</b>
                    <b>
                      {i.mods.length} {i.mods.length === 1 ? "mod" : "mods"}
                    </b>
                  </div>
                  <div className="inst-played">
                    Last played {i.last_played?.slice(0, 10) ?? "never"}
                  </div>
                </div>
              </div>

              <div className="inst-bar">
                <span
                  className="launch-wrap"
                  title={
                    account ? undefined : "Sign in with Microsoft to launch"
                  }
                >
                  <button
                    className="btn"
                    onClick={() => launch(i.id)}
                    disabled={!account}
                  >
                    Launch
                  </button>
                </span>
                <button
                  className="btn ghost"
                  onClick={() =>
                    setExpanded((e) => (e === i.id ? null : i.id))
                  }
                >
                  {expanded === i.id ? "Hide" : "Mods"}
                </button>
                <button
                  className="btn ghost"
                  onClick={() => openUpdates(i)}
                >
                  Updates
                </button>
                <button
                  className="btn ghost"
                  title="Open this pack's curator chat"
                  onClick={() => onOpenChat(i.id, i.name)}
                >
                  Chat
                </button>
                <button
                  className="btn ghost"
                  title="View and edit keybinds for this pack"
                  onClick={() => openKeybinds(i)}
                >
                  Keys
                </button>
                <button
                  className="btn ghost"
                  title="Boot the pack once and check mods load — no need to play"
                  disabled={!account || verifying !== null}
                  onClick={() => verify(i.id)}
                >
                  {verifying === i.id ? "Verifying…" : "Verify"}
                </button>
                <button
                  className="inst-iconbtn"
                  title="Duplicate"
                  aria-label="Duplicate instance"
                  onClick={() => doDuplicate(i)}
                >
                  ⧉
                </button>
                <button
                  className="inst-iconbtn danger"
                  title="Delete"
                  aria-label="Delete instance"
                  onClick={() => setConfirmDelete(i)}
                >
                  ×
                </button>
              </div>

              {verifyMsg[i.id] && (
                <div className="card-hint" style={{ paddingTop: 6 }}>
                  {verifyMsg[i.id]}
                </div>
              )}

              {expanded === i.id && (
                <div className="inst-mods">
                  {i.mods.length === 0 ? (
                    <div className="card-hint">No mods in this pack.</div>
                  ) : (
                    i.mods.map((m) => (
                      <div className="inst-mod-row" key={m.project_id}>
                        <span>{m.name}</span>
                        <button
                          className="mod-x"
                          aria-label={`Remove ${m.name}`}
                          onClick={() => removeMod(i.id, m.project_id)}
                        >
                          ×
                        </button>
                      </div>
                    ))
                  )}
                </div>
              )}
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

      {showCreate && (
        <div className="overlay" onClick={() => setShowCreate(false)}>
          <div className="modal" onClick={(e) => e.stopPropagation()}>
            <h3>New instance</h3>
            <div className="field">
              <span className="field-label">Name</span>
              <input
                autoFocus
                placeholder="My pack"
                value={nName}
                onChange={(e) => setNName(e.target.value)}
              />
            </div>
            <div className="field">
              <span className="field-label">Minecraft version</span>
              <Dropdown value={nMc} options={MC_OPTS} onChange={setNMc} />
            </div>
            <div className="field">
              <span className="field-label">Loader</span>
              <Dropdown
                value={nLoader}
                options={LOADER_OPTS}
                onChange={setNLoader}
              />
            </div>
            <div className="field">
              <span className="field-label">Loader version (optional)</span>
              <input
                placeholder="leave blank for latest"
                value={nLoaderVer}
                onChange={(e) => setNLoaderVer(e.target.value)}
              />
            </div>
            {createError && <div className="error">{createError}</div>}
            <div className="modal-actions">
              <button
                className="btn ghost"
                onClick={() => setShowCreate(false)}
              >
                Cancel
              </button>
              <button
                className="btn"
                onClick={doCreate}
                disabled={createBusy || !nName.trim()}
              >
                {createBusy ? "Creating." : "Create"}
              </button>
            </div>
          </div>
        </div>
      )}

      {confirmDelete && (
        <div className="overlay" onClick={() => setConfirmDelete(null)}>
          <div className="modal" onClick={(e) => e.stopPropagation()}>
            <h3>Delete instance</h3>
            <p className="modal-text">
              Delete "{confirmDelete.name}"? This cannot be undone.
            </p>
            <div className="modal-actions">
              <button
                className="btn ghost"
                onClick={() => setConfirmDelete(null)}
              >
                Cancel
              </button>
              <button
                className="btn danger-btn"
                onClick={() => doDelete(confirmDelete)}
              >
                Delete
              </button>
            </div>
          </div>
        </div>
      )}

      {dupTarget && (
        <div className="overlay" onClick={() => setDupTarget(null)}>
          <div className="modal" onClick={(e) => e.stopPropagation()}>
            <h3>Duplicate instance</h3>
            <div className="field">
              <span className="field-label">Name for the copy</span>
              <input
                autoFocus
                value={dupName}
                onChange={(e) => setDupName(e.target.value)}
                onKeyDown={(e) => {
                  if (e.key === "Enter") confirmDuplicate();
                }}
              />
            </div>
            {dupError && <div className="error">{dupError}</div>}
            <div className="modal-actions">
              <button
                className="btn ghost"
                onClick={() => setDupTarget(null)}
              >
                Cancel
              </button>
              <button
                className="btn"
                onClick={confirmDuplicate}
                disabled={dupBusy || !dupName.trim()}
              >
                {dupBusy ? "Duplicating." : "Duplicate"}
              </button>
            </div>
          </div>
        </div>
      )}

      {updTarget && (
        <div className="overlay" onClick={() => setUpdTarget(null)}>
          <div className="modal" onClick={(e) => e.stopPropagation()}>
            <h3>Updates for {updTarget.name}</h3>
            {updBusy && !updates && (
              <p className="modal-text">Checking.</p>
            )}
            {updError && <div className="error">{updError}</div>}
            {updates && updates.length === 0 && (
              <p className="modal-text">Everything is up to date.</p>
            )}
            {updates && updates.length > 0 && (
              <div className="upd-list">
                {updates.map((u) => (
                  <button
                    type="button"
                    key={u.project_id}
                    className={`upd-row ${
                      updSel.includes(u.project_id) ? "on" : ""
                    }`}
                    onClick={() => toggleUpd(u.project_id)}
                  >
                    <span className="dd-box">
                      {updSel.includes(u.project_id) ? "✓" : ""}
                    </span>
                    <span className="upd-name">{u.name}</span>
                    <span className="upd-ver">
                      {u.from} -&gt; {u.to}
                    </span>
                  </button>
                ))}
              </div>
            )}
            <div className="modal-actions">
              <button
                className="btn ghost"
                onClick={() => setUpdTarget(null)}
              >
                Close
              </button>
              {updates && updates.length > 0 && (
                <button
                  className="btn"
                  onClick={applyUpdates}
                  disabled={updBusy || updSel.length === 0}
                >
                  {updBusy ? "Applying." : "Apply selected"}
                </button>
              )}
            </div>
          </div>
        </div>
      )}

      {kbTarget && (
        <div className="overlay" onClick={closeKeybinds}>
          <div
            className="kb-modal"
            onClick={(e) => e.stopPropagation()}
          >
            <div className="kb-head">
              <h3>Keybinds — {kbTarget.name}</h3>
              <button
                className="inst-iconbtn"
                aria-label="Close"
                onClick={closeKeybinds}
              >
                ×
              </button>
            </div>

            {kbBusy && <div className="spinner">Reading keybinds.</div>}
            {kbError && <div className="error">{kbError}</div>}

            {!kbBusy && kbReport && !kbReport.launched && (
              <div className="placeholder">
                <strong>Launch this pack once first</strong>
                Mods register their keybinds when the game runs. Play the pack
                once, then they'll all show up here to organise and rebind.
              </div>
            )}

            {!kbBusy && kbReport && kbReport.launched && (
              <>
                {kbReport.conflict_count > 0 && (
                  <div className="kb-banner">
                    {kbReport.conflict_count} key
                    {kbReport.conflict_count === 1 ? "" : "s"} bound to more
                    than one action — shown in red below.
                  </div>
                )}
                <div className="kb-list">
                  {kbReport.groups.map((g) => (
                    <div className="kb-group" key={g.mod_name}>
                      <div className="kb-group-title">{g.mod_name}</div>
                      {g.binds.map((b) => {
                        const edited = b.name in kbEdits;
                        const token = edited ? kbEdits[b.name] : b.key_token;
                        const display = edited
                          ? tokenDisplay(token)
                          : b.key_display;
                        const capturing = kbCapturing === b.name;
                        return (
                          <div
                            className={
                              "kb-row" + (b.conflict ? " kb-conflict" : "")
                            }
                            key={b.name}
                          >
                            <span className="kb-label">{b.label}</span>
                            <button
                              className={
                                "kb-key" +
                                (capturing ? " kb-capturing" : "") +
                                (edited ? " kb-edited" : "")
                              }
                              onClick={() => setKbCapturing(b.name)}
                              title="Click, then press a key or mouse button"
                            >
                              {capturing ? "press a key…" : display}
                            </button>
                            <button
                              className="kb-unbind"
                              title="Unbind"
                              onClick={() =>
                                setKbEdits((m) => ({
                                  ...m,
                                  [b.name]: "key.keyboard.unknown",
                                }))
                              }
                            >
                              ⌫
                            </button>
                          </div>
                        );
                      })}
                    </div>
                  ))}
                </div>
                <div className="kb-foot">
                  <span className="kb-dirty">
                    {Object.keys(kbEdits).length > 0
                      ? `${Object.keys(kbEdits).length} unsaved change${
                          Object.keys(kbEdits).length === 1 ? "" : "s"
                        }`
                      : ""}
                  </span>
                  <button className="btn ghost" onClick={closeKeybinds}>
                    Cancel
                  </button>
                  <button
                    className="btn"
                    onClick={saveKeybinds}
                    disabled={kbBusy || Object.keys(kbEdits).length === 0}
                  >
                    Save
                  </button>
                </div>
              </>
            )}
          </div>
        </div>
      )}
    </>
  );
}
