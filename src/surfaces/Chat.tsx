import {
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
} from "react";
import { api, ChatMessage, CuratorEvent, on } from "../lib/api";
import type { Surface } from "../App";

const EXAMPLES = [
  "Cozy Create pack for 1.21 Fabric, runs on a laptop",
  "Hardcore exploration pack, NeoForge 1.21.1",
  "Kitchen-sink tech+magic for me and 3 friends",
];

interface Assembled {
  instanceId: string;
  name: string;
}

export default function Chat({
  onNavigate,
}: {
  onNavigate: (s: Surface) => void;
}) {
  const [hasKey, setHasKey] = useState<boolean | null>(null);
  const [history, setHistory] = useState<ChatMessage[]>([]);
  const [draft, setDraft] = useState("");
  const [streaming, setStreaming] = useState(false);
  const [activity, setActivity] = useState<string | null>(null);
  const [assembled, setAssembled] = useState<Assembled | null>(null);
  const [error, setError] = useState<string | null>(null);

  const scrollRef = useRef<HTMLDivElement | null>(null);
  const taRef = useRef<HTMLTextAreaElement | null>(null);

  useEffect(() => {
    let alive = true;
    api
      .getSettings()
      .then((s) => alive && setHasKey(s.has_anthropic_key))
      .catch(() => alive && setHasKey(false));
    return () => {
      alive = false;
    };
  }, []);

  // Stream the curator turn. Append text deltas to the trailing assistant
  // message; clear the tool pill once real text starts flowing.
  useEffect(() => {
    const p = on<CuratorEvent>("curator-event", (e) => {
      switch (e.kind) {
        case "text": {
          const delta = e["0"];
          setActivity(null);
          setHistory((h) => {
            const last = h[h.length - 1];
            if (!last || last.role !== "assistant") return h;
            return [
              ...h.slice(0, -1),
              { ...last, content: last.content + delta },
            ];
          });
          break;
        }
        case "tool":
          setActivity(`${e.name} · ${e.status}`);
          break;
        case "assembled":
          setAssembled({ instanceId: e.instance_id, name: e.name });
          break;
        case "done":
          setActivity(null);
          setStreaming(false);
          break;
        case "error":
          setActivity(null);
          setStreaming(false);
          setError(e["0"]);
          break;
      }
    });
    return () => {
      p.then((un) => un());
    };
  }, []);

  useLayoutEffect(() => {
    if (scrollRef.current)
      scrollRef.current.scrollTop = scrollRef.current.scrollHeight;
  }, [history, activity, assembled]);

  function autoGrow() {
    const ta = taRef.current;
    if (!ta) return;
    ta.style.height = "auto";
    ta.style.height = Math.min(ta.scrollHeight, 200) + "px";
  }

  async function send() {
    const message = draft.trim();
    if (!message || streaming) return;
    setError(null);
    setAssembled(null);
    const base = history;
    setHistory([
      ...base,
      { role: "user", content: message },
      { role: "assistant", content: "" },
    ]);
    setDraft("");
    setStreaming(true);
    if (taRef.current) taRef.current.style.height = "auto";
    try {
      await api.curatorSend(base, message);
    } catch (e) {
      setStreaming(false);
      setError(String(e));
    }
  }

  function onKeyDown(e: React.KeyboardEvent<HTMLTextAreaElement>) {
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      send();
    }
  }

  function applyExample(text: string) {
    setDraft(text);
    requestAnimationFrame(() => {
      taRef.current?.focus();
      autoGrow();
    });
  }

  if (hasKey === false) {
    return (
      <>
        <div className="page-head">
          <h2>Curator</h2>
        </div>
        <div className="placeholder">
          <strong>Add an Anthropic API key</strong>
          Set your key in{" "}
          <button
            className="link-btn"
            onClick={() => onNavigate("settings")}
          >
            Settings
          </button>{" "}
          to use the curator.
        </div>
      </>
    );
  }

  const empty = history.length === 0;

  return (
    <div className="chat">
      <div className="chat-scroll" ref={scrollRef}>
        {empty ? (
          <div className="chat-empty">
            <h2 className="chat-empty-title">Design a modpack</h2>
            <div className="chat-chips">
              {EXAMPLES.map((ex) => (
                <button
                  key={ex}
                  className="chip"
                  onClick={() => applyExample(ex)}
                >
                  {ex}
                </button>
              ))}
            </div>
          </div>
        ) : (
          <div className="chat-col">
            {history.map((m, i) =>
              m.role === "user" ? (
                <div className="chat-row-user" key={i}>
                  <div className="chat-msg-user">{m.content}</div>
                </div>
              ) : (
                <div className="chat-msg-assistant" key={i}>
                  {m.content}
                  {streaming &&
                    i === history.length - 1 &&
                    m.content === "" &&
                    !activity && <span className="caret" />}
                </div>
              ),
            )}

            {activity && (
              <div className="tool-pill">
                <span className="tool-dot" /> {activity}
              </div>
            )}

            {assembled && (
              <div className="assembled-note">
                <span>Created instance: {assembled.name}</span>
                <button
                  className="btn"
                  onClick={() => onNavigate("instances")}
                >
                  View in Instances
                </button>
              </div>
            )}

            {error && <div className="error">{error}</div>}
          </div>
        )}
      </div>

      <div className="composer-wrap">
        <div className="composer">
          <textarea
            ref={taRef}
            value={draft}
            placeholder="Describe the modpack you want"
            rows={1}
            disabled={streaming}
            onChange={(e) => {
              setDraft(e.target.value);
              autoGrow();
            }}
            onKeyDown={onKeyDown}
          />
          <button
            className="composer-send"
            onClick={send}
            disabled={streaming || !draft.trim()}
            aria-label="Send"
          >
            ↑
          </button>
        </div>
      </div>
    </div>
  );
}
