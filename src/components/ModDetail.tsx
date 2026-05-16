import { useEffect, useState } from "react";
import { api, ModDetail as Detail } from "../lib/api";

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
              <div className="body-text">{p.body || p.description}</div>

              <div className="section-label">
                Versions ({data.versions.length})
              </div>
              <table className="versions">
                <thead>
                  <tr>
                    <th>Version</th>
                    <th>Channel</th>
                    <th>MC</th>
                    <th>Loaders</th>
                    <th>Published</th>
                  </tr>
                </thead>
                <tbody>
                  {data.versions.slice(0, 12).map((v) => (
                    <tr key={v.id}>
                      <td>{v.version_number}</td>
                      <td>{v.version_type}</td>
                      <td>{v.game_versions.slice(0, 3).join(", ")}</td>
                      <td>{v.loaders.join(", ")}</td>
                      <td>{v.date_published.slice(0, 10)}</td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>

            <div className="detail-side">
              <button
                className="btn"
                style={{ width: "100%", marginBottom: 18 }}
                disabled
              >
                Add to instance
              </button>

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
