import { useEffect, useMemo, useState } from "react";
import { marked } from "marked";
import DOMPurify from "dompurify";
import { openUrl } from "@tauri-apps/plugin-opener";
import { api, Instance, ModDetail as Detail, formatEditError } from "../lib/api";
import { Dropdown, Opt } from "./Dropdown";

/** Modrinth-style mod detail: description + versions table on the left,
 *  side-info (categories, links, environment, license) on the right. */
export default function ModDetail({
  idOrSlug,
  onClose,
}: {
  idOrSlug: string;
  onClose: () => void;
}) {
  const [data, setData] = useState<Detail | null>(null);
  const [error, setError] = useState<string | null>(null);

  const [instances, setInstances] = useState<Instance[]>([]);
  const [pick, setPick] = useState("");
  const [adding, setAdding] = useState(false);
  const [added, setAdded] = useState(false);
  const [addError, setAddError] = useState<string | null>(null);
  // null = Auto (backend picks the best compatible version). Non-null
  // pins a specific version id. Default Auto keeps adding one click.
  const [selectedVer, setSelectedVer] = useState<string | null>(null);

  useEffect(() => {
    let alive = true;
    api
      .getMod(idOrSlug)
      .then((d) => alive && setData(d))
      .catch((e) => alive && setError(String(e)));
    return () => {
      alive = false;
    };
  }, [idOrSlug]);

  useEffect(() => {
    let alive = true;
    api
      .listInstances()
      .then((list) => {
        if (!alive) return;
        setInstances(list);
        if (list.length) setPick(list[0].id);
      })
      .catch(() => {});
    return () => {
      alive = false;
    };
  }, []);

  async function addToInstance() {
    const p = data?.project;
    if (!p || !pick) return;
    setAdding(true);
    setAddError(null);
    try {
      await api.addModToInstance(pick, p.id, selectedVer ?? undefined);
      setAdded(true);
      setTimeout(() => setAdded(false), 2000);
    } catch (e) {
      setAddError(formatEditError(e));
    } finally {
      setAdding(false);
    }
  }

  const instOpts: Opt[] = instances.map((i) => ({
    value: i.id,
    label: i.name,
  }));

  // Modrinth bodies are Markdown with embedded HTML. Render then sanitize.
  const bodyHtml = useMemo(() => {
    const src = data?.project?.body || data?.project?.description || "";
    if (!src) return "";
    return DOMPurify.sanitize(marked.parse(src, { async: false }) as string);
  }, [data]);

  // Keep links from navigating the app webview; open them in the browser.
  function openLink(e: React.MouseEvent<HTMLDivElement>) {
    const a = (e.target as HTMLElement).closest("a");
    const href = a?.getAttribute("href");
    if (href && /^https?:/i.test(href)) {
      e.preventDefault();
      openUrl(href).catch(() => {});
    }
  }

  const p = data?.project;

  return (
    <div className="overlay" onClick={onClose}>
      <div
        className="detail"
        style={{ position: "relative" }}
        onClick={(e) => e.stopPropagation()}
      >
        <button className="close-x" onClick={onClose} aria-label="Close">
          ×
        </button>

        {error && (
          <div className="detail-main">
            <div className="error">Failed to load: {error}</div>
          </div>
        )}

        {!error && !data && (
          <div className="detail-main">
            <div className="spinner">Loading mod…</div>
          </div>
        )}

        {p && data && (
          <>
            <div className="detail-main">
              <div className="detail-head">
                {p.icon_url && <img src={p.icon_url} alt="" />}
                <div>
                  <h2>{p.title}</h2>
                  <div className="by" style={{ color: "var(--muted)" }}>
                    ↓ {p.downloads.toLocaleString()} · ♥ {p.followers}
                  </div>
                </div>
              </div>

              <div>
                {p.categories.map((c) => (
                  <span className="tag" key={c}>
                    {c}
                  </span>
                ))}
              </div>

              <div className="section-label">Description</div>
              <div
                className="body-text md"
                onClick={openLink}
                dangerouslySetInnerHTML={{ __html: bodyHtml }}
              />

              <div className="section-label versions-label">
                <span>Versions ({data.versions.length})</span>
                <span className="versions-pick">
                  {selectedVer ? (
                    <button
                      type="button"
                      className="ver-clear"
                      onClick={() => setSelectedVer(null)}
                    >
                      Using selected · reset to Auto
                    </button>
                  ) : (
                    <span className="ver-auto">Auto (newest compatible)</span>
                  )}
                </span>
              </div>
              <table className="versions selectable">
                <thead>
                  <tr>
                    <th aria-label="Selected"></th>
                    <th>Version</th>
                    <th>Channel</th>
                    <th>MC</th>
                    <th>Loaders</th>
                    <th>Published</th>
                  </tr>
                </thead>
                <tbody>
                  {data.versions.slice(0, 12).map((v) => {
                    const sel = selectedVer === v.id;
                    return (
                      <tr
                        key={v.id}
                        className={sel ? "sel" : ""}
                        onClick={() =>
                          setSelectedVer(sel ? null : v.id)
                        }
                        title={
                          sel
                            ? "Selected — click to use Auto instead"
                            : "Click to pin this version"
                        }
                      >
                        <td className="ver-check">{sel ? "✓" : ""}</td>
                        <td>{v.version_number}</td>
                        <td>{v.version_type}</td>
                        <td>{v.game_versions.slice(0, 3).join(", ")}</td>
                        <td>{v.loaders.join(", ")}</td>
                        <td>{v.date_published.slice(0, 10)}</td>
                      </tr>
                    );
                  })}
                </tbody>
              </table>
            </div>

            <div className="detail-side">
              <div className="add-inst">
                {instances.length === 0 ? (
                  <div className="card-hint">Create an instance first.</div>
                ) : (
                  <>
                    <Dropdown
                      value={pick}
                      options={instOpts}
                      onChange={setPick}
                    />
                    <button
                      className="btn"
                      onClick={addToInstance}
                      disabled={adding || !pick}
                    >
                      {adding ? "Adding." : added ? "Added" : "Add"}
                    </button>
                  </>
                )}
              </div>
              {addError && (
                <div
                  className="error"
                  style={{ marginBottom: 14, whiteSpace: "pre-line" }}
                >
                  {addError}
                </div>
              )}

              <div className="section-label">Environment</div>
              <div className="side-row">
                <span>Client</span>
                <span>{p.client_side}</span>
              </div>
              <div className="side-row">
                <span>Server</span>
                <span>{p.server_side}</span>
              </div>

              <div className="section-label">License</div>
              <div className="side-row">
                <span>{p.license.name || p.license.id}</span>
                {p.license.url && (
                  <a href={p.license.url} target="_blank" rel="noreferrer">
                    view
                  </a>
                )}
              </div>

              <div className="section-label">Links</div>
              {p.source_url && (
                <div className="side-row">
                  <span>Source</span>
                  <a href={p.source_url} target="_blank" rel="noreferrer">
                    open
                  </a>
                </div>
              )}
              {p.issues_url && (
                <div className="side-row">
                  <span>Issues</span>
                  <a href={p.issues_url} target="_blank" rel="noreferrer">
                    open
                  </a>
                </div>
              )}
              {p.wiki_url && (
                <div className="side-row">
                  <span>Wiki</span>
                  <a href={p.wiki_url} target="_blank" rel="noreferrer">
                    open
                  </a>
                </div>
              )}
            </div>
          </>
        )}
      </div>
    </div>
  );
}
