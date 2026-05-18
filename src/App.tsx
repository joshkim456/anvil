import { useEffect, useState } from "react";
import { api, AppInfo } from "./lib/api";
import { applyTheme } from "./lib/theme";
import Browse from "./surfaces/Browse";
import Chat from "./surfaces/Chat";
import Instances from "./surfaces/Instances";
import Quests from "./surfaces/Quests";
import Settings from "./surfaces/Settings";

export type Surface =
  | "chat"
  | "browse"
  | "instances"
  | "quests"
  | "settings";

const NAV: { id: Surface; label: string; glyph: string }[] = [
  { id: "chat", label: "Curator", glyph: "✦" },
  { id: "browse", label: "Browse mods", glyph: "⌗" },
  { id: "instances", label: "Instances", glyph: "▦" },
  { id: "quests", label: "Progression", glyph: "✶" },
  { id: "settings", label: "Settings", glyph: "⚙" },
];

export interface ChatRequest {
  instanceId: string;
  name: string;
}

export default function App() {
  const [surface, setSurface] = useState<Surface>("browse");
  const [info, setInfo] = useState<AppInfo | null>(null);
  // A request from the Instances surface to open that instance's chat thread.
  const [chatReq, setChatReq] = useState<ChatRequest | null>(null);

  useEffect(() => {
    api.appInfo().then(setInfo).catch(() => {});
    api
      .getSettings()
      .then((s) => applyTheme(s.theme))
      .catch(() => {});
  }, []);

  return (
    <div className="shell">
      <nav className="rail">
        <div className="brand">
          <h1>Anvil</h1>
          <span className="dot" />
        </div>
        {NAV.map((n) => (
          <button
            key={n.id}
            className={`nav-item ${surface === n.id ? "active" : ""}`}
            onClick={() => setSurface(n.id)}
          >
            <span aria-hidden>{n.glyph}</span>
            {n.label}
          </button>
        ))}
        <div className="rail-foot">
          {info ? `${info.name} v${info.version}` : ""}
        </div>
      </nav>

      <main className="main">
        {/* Chat stays mounted (just hidden) so the conversation and any
            in-flight stream survive tab switches. */}
        <div style={{ display: surface === "chat" ? "contents" : "none" }}>
          <Chat
            onNavigate={setSurface}
            openInstance={chatReq}
            onConsumed={() => setChatReq(null)}
          />
        </div>
        {surface === "browse" && <Browse />}
        {surface === "instances" && (
          <Instances
            onOpenChat={(instanceId, name) => {
              setChatReq({ instanceId, name });
              setSurface("chat");
            }}
          />
        )}
        {surface === "quests" && <Quests />}
        {surface === "settings" && <Settings />}
      </main>
    </div>
  );
}
