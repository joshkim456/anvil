import { useEffect, useState } from "react";
import { api, ThemePref } from "../lib/api";
import { applyTheme } from "../lib/theme";
import { Dropdown, Opt } from "../components/Dropdown";

const THEME_OPTS: Opt[] = [
  { value: "system", label: "System" },
  { value: "light", label: "Light" },
  { value: "dark", label: "Dark" },
];

export default function Settings() {
  const [hasKey, setHasKey] = useState(false);
  const [apiKey, setApiKey] = useState("");
  const [clientId, setClientId] = useState("");
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [saved, setSaved] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [theme, setTheme] = useState<ThemePref>("system");

  useEffect(() => {
    let alive = true;
    api
      .getSettings()
      .then((s) => {
        if (!alive) return;
        setHasKey(s.has_anthropic_key);
        setClientId(s.ms_client_id ?? "");
        setTheme(s.theme);
      })
      .catch((e) => alive && setError(String(e)))
      .finally(() => alive && setLoading(false));
    return () => {
      alive = false;
    };
  }, []);

  async function save() {
    setSaving(true);
    setSaved(false);
    setError(null);
    const args: { anthropicApiKey?: string; msClientId?: string } = {};
    // Blank password field with a key already set means leave it untouched.
    if (apiKey.trim()) args.anthropicApiKey = apiKey.trim();
    args.msClientId = clientId.trim();
    try {
      await api.setSettings(args);
      if (apiKey.trim()) {
        setHasKey(true);
        setApiKey("");
      }
      setSaved(true);
    } catch (e) {
      setError(String(e));
    } finally {
      setSaving(false);
    }
  }

  // Theme applies instantly (and persists) — no Save round-trip needed.
  async function changeTheme(t: ThemePref) {
    setTheme(t);
    applyTheme(t);
    try {
      await api.setSettings({ theme: t });
    } catch (e) {
      setError(String(e));
    }
  }

  return (
    <>
      <div className="page-head">
        <h2>Settings</h2>
      </div>

      {loading ? (
        <div className="spinner">Loading settings…</div>
      ) : (
        <div className="settings-form">
          {error && <div className="error">{error}</div>}

          <div className="field">
            <span className="field-label">Theme</span>
            <Dropdown
              value={theme}
              options={THEME_OPTS}
              onChange={(v) => changeTheme(v as ThemePref)}
            />
            <span className="field-hint">
              System follows your OS appearance.
            </span>
          </div>

          <label className="field">
            <span className="field-label">
              Anthropic API key
              {hasKey && <span className="field-badge">key set</span>}
            </span>
            <input
              type="password"
              value={apiKey}
              placeholder={hasKey ? "Enter a new key to replace" : "sk-ant-…"}
              onChange={(e) => setApiKey(e.target.value)}
              autoComplete="off"
            />
          </label>

          <label className="field">
            <span className="field-label">Microsoft client ID</span>
            <input
              type="text"
              value={clientId}
              placeholder="00000000-0000-0000-0000-000000000000"
              onChange={(e) => setClientId(e.target.value)}
              autoComplete="off"
            />
            <span className="field-hint">
              Reserved for a future Microsoft-approved sign-in. Current sign-in
              does not use this field.
            </span>
          </label>

          <div className="settings-actions">
            <button className="btn" onClick={save} disabled={saving}>
              {saving ? "Saving…" : "Save"}
            </button>
            {saved && <span className="field-saved">Saved</span>}
          </div>
        </div>
      )}
    </>
  );
}
