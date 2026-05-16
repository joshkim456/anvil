import { useEffect, useState } from "react";
import { api, AppInfo } from "./lib/api";
import Browse from "./surfaces/Browse";
import Chat from "./surfaces/Chat";
import Instances from "./surfaces/Instances";
import Settings from "./surfaces/Settings";

export type Surface = "chat" | "browse" | "instances" | "settings";

const NAV: { id: Surface; label: string; glyph: string }[] = [
  { id: "chat", label: "Curator", glyph: "✦" },
  { id: "browse", label: "Browse mods", glyph: "⌗" },
  { id: "instances", label: "Instances", glyph: "▦" },
  { id: "settings", label: "Settings", glyph: "⚙" },
];

export default function App() {
  const [surface, setSurface] = useState<Surface>("browse");
  const [info, setInfo] = useState<AppInfo | null>(null);

  useEffect(() => {
    api.appInfo().then(setInfo).catch(() => {});
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
        {surface === "chat" && <Chat onNavigate={setSurface} />}
        {surface === "browse" && <Browse />}
        {surface === "instances" && <Instances />}
        {surface === "settings" && <Settings />}
      </main>
    </div>
  );
}
