import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { marked } from "marked";
import DOMPurify from "dompurify";
import { openUrl } from "@tauri-apps/plugin-opener";
import {
  api,
  ChatMessage,
  ChatThread,
  CuratorEvent,
  on,
  Phase,
} from "../lib/api";
import { Dropdown, Opt } from "../components/Dropdown";
import type { ChatRequest, Surface } from "../App";

// Curator replies are Markdown. Render + sanitize (single newlines -> breaks).
function renderMd(src: string): string {
  return DOMPurify.sanitize(
    marked.parse(src, { async: false, breaks: true }) as string,
  );
}

function openExternal(e: React.MouseEvent<HTMLDivElement>) {
  const a = (e.target as HTMLElement).closest("a");
  const href = a?.getAttribute("href");
  if (href && /^https?:/i.test(href)) {
    e.preventDefault();
    openUrl(href).catch(() => {});
  }
}

const EXAMPLES: { tag: string; text: string }[] = [
  { tag: "Cozy", text: "Cozy Create pack for 1.21 Fabric, runs on a laptop" },
  { tag: "Hardcore", text: "Hardcore exploration pack, NeoForge 1.21.1" },
  { tag: "Multiplayer", text: "Kitchen-sink tech and magic for me and 3 friends" },
  { tag: "Story", text: "Adventure pack with a questline, medium difficulty" },
];

// If the backend goes silent (bad key, network), surface it instead of hanging.
const SILENCE_MS = 45000;

interface Assembled {
  instanceId: string;
  name: string;
}

const genId = () =>
  Date.now().toString(36) + Math.random().toString(36).slice(2, 7);

function deriveTitle(messages: ChatMessage[]): string {
  const first = messages.find((m) => m.role === "user")?.content.trim();
  if (!first) return "New chat";
  return first.length > 42 ? first.slice(0, 42) + "…" : first;
}

export default function Chat({
  onNavigate,
  openInstance,
  onConsumed,
  chatsRefresh,
}: {
  onNavigate: (s: Surface) => void;
  openInstance: ChatRequest | null;
  onConsumed: () => void;
  chatsRefresh: number;
}) {
  const [hasKey, setHasKey] = useState<boolean | null>(null);
  const [history, setHistory] = useState<ChatMessage[]>([]);
  const [draft, setDraft] = useState("");
  const [streaming, setStreaming] = useState(false);
  const [activity, setActivity] = useState<string | null>(null);
  const [assembled, setAssembled] = useState<Assembled | null>(null);
  const [error, setError] = useState<string | null>(null);

  // Thread layer. The conversation lives in a persisted ChatThread; once the
  // curator assembles a pack the thread binds to that instance id.
  const [threads, setThreads] = useState<ChatThread[]>([]);
  const [activeId, setActiveId] = useState<string>(genId);
  const [boundInstance, setBoundInstance] = useState<string | null>(null);
  const [title, setTitle] = useState("");
  const [phase, setPhase] = useState<Phase>("curating");
  const createdRef = useRef<string>(new Date().toISOString());

  // The curator-event listener is registered once; it reads thread identity
  // through refs so it always persists against the *current* thread.
  const metaRef = useRef({ activeId, boundInstance, title, phase });
  metaRef.current = { activeId, boundInstance, title, phase };
  // Latest messages, so the (once-registered) event listener can persist
  // without nesting setState inside a setHistory updater.
  const historyRef = useRef<ChatMessage[]>(history);
  historyRef.current = history;

  const scrollRef = useRef<HTMLDivElement | null>(null);
  const taRef = useRef<HTMLTextAreaElement | null>(null);
  const silenceRef = useRef<number | null>(null);

  function clearSilence() {
    if (silenceRef.current) {
      window.clearTimeout(silenceRef.current);
      silenceRef.current = null;
    }
  }

  function armSilence() {
    clearSilence();
    silenceRef.current = window.setTimeout(() => {
      setStreaming(false);
      setActivity(null);
      setError(
        "No response from the curator. Check that your Anthropic API key in Settings is valid, then try again.",
      );
    }, SILENCE_MS);
  }

  // Persist the active thread to disk and hoist it to the top of the list.
  function persist(
    messages: ChatMessage[],
    over?: { instanceId?: string; title?: string; phase?: Phase },
  ) {
    const m = metaRef.current;
    if (messages.length === 0) return; // never write an empty draft
    const t: ChatThread = {
      id: m.activeId,
      instance_id: over?.instanceId ?? m.boundInstance,
      title: over?.title || m.title || deriveTitle(messages),
      created: createdRef.current,
      updated: new Date().toISOString(),
      phase: over?.phase ?? m.phase,
      messages,
    };
    if (over?.instanceId) setBoundInstance(over.instanceId);
    if (over?.phase && over.phase !== m.phase) setPhase(over.phase);
    if (t.title !== m.title) setTitle(t.title);
    setThreads((prev) => [t, ...prev.filter((x) => x.id !== t.id)]);
    api.saveChat(t).catch(() => {});
  }

  function loadThread(t: ChatThread) {
    clearSilence();
    setActiveId(t.id);
    createdRef.current = t.created;
    setBoundInstance(t.instance_id);
    setTitle(t.title);
    setPhase(t.phase ?? "curating");
    setHistory(t.messages);
    setStreaming(false);
    setActivity(null);
    setAssembled(null);
    setError(null);
  }

  function newThread() {
    clearSilence();
    setActiveId(genId());
    createdRef.current = new Date().toISOString();
    setBoundInstance(null);
    setTitle("");
    setPhase("curating");
    setHistory([]);
    setStreaming(false);
    setActivity(null);
    setAssembled(null);
    setError(null);
  }

  useEffect(() => {
    let alive = true;
    api
      .getSettings()
      .then((s) => alive && setHasKey(s.has_anthropic_key))
      .catch(() => alive && setHasKey(false));
    api
      .listChats()
      .then((list) => alive && setThreads(list))
      .catch(() => {});
    return () => {
      alive = false;
      clearSilence();
    };
  }, []);

  // An instance was deleted while this surface stayed mounted; its bound
  // thread file is gone on disk, so re-fetch to drop the stale entry. The
  // mount effect above already does the initial load (skip bump 0).
  useEffect(() => {
    if (chatsRefresh === 0) return;
    let alive = true;
    api
      .listChats()
      .then((list) => alive && setThreads(list))
      .catch(() => {});
    return () => {
      alive = false;
    };
  }, [chatsRefresh]);

  // Instances asked to open a specific instance's thread: reuse the existing
  // one if there is one, else start a fresh thread pre-bound to that instance.
  useEffect(() => {
    if (!openInstance) return;
    const { instanceId, name } = openInstance;
    let alive = true;
    api
      .chatForInstance(instanceId)
      .then((t) => {
        if (!alive) return;
        if (t) {
          loadThread(t);
        } else {
          newThread();
          setBoundInstance(instanceId);
          setTitle(name);
          // The instance already exists, so this thread starts post-assembly.
          setPhase("assembled");
        }
      })
      .catch(() => {})
      .finally(() => alive && onConsumed());
    return () => {
      alive = false;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [openInstance]);

  useEffect(() => {
    const p = on<CuratorEvent>("curator-event", (e) => {
      switch (e.kind) {
        case "text": {
          armSilence();
          setActivity(null);
          setHistory((h) => {
            const last = h[h.length - 1];
            if (!last || last.role !== "assistant") return h;
            return [
              ...h.slice(0, -1),
              { ...last, content: last.content + e.data },
            ];
          });
          break;
        }
        case "tool":
          armSilence();
          setActivity(`${e.data.name} · ${e.data.status}`);
          break;
        case "assembled":
          armSilence();
          setAssembled({
            instanceId: e.data.instance_id,
            name: e.data.name,
          });
          persist(historyRef.current, {
            instanceId: e.data.instance_id,
            title: e.data.name,
          });
          break;
        case "phase":
          // Rust state machine advanced the pipeline; reflect it live and
          // persist so the next turn (and a reopened thread) is scoped to it.
          setPhase(e.data as Phase);
          persist(historyRef.current, { phase: e.data as Phase });
          break;
        case "usage":
          // Observability only: per-round token + cache counts. Logged, not
          // rendered, so cache hit/miss is inspectable without UI noise.
          console.debug("curator usage", e.data);
          break;
        case "done":
          clearSilence();
          setActivity(null);
          setStreaming(false);
          // Defer so the final text delta has committed to historyRef.
          setTimeout(() => persist(historyRef.current), 0);
          break;
        case "error":
          clearSilence();
          setActivity(null);
          setStreaming(false);
          setError(e.data);
          break;
      }
    });
    return () => {
      p.then((un) => un());
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  useLayoutEffect(() => {
    if (scrollRef.current)
      scrollRef.current.scrollTop = scrollRef.current.scrollHeight;
  }, [history, activity, assembled, error]);

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
    const next: ChatMessage[] = [
      ...base,
      { role: "user", content: message },
      { role: "assistant", content: "" },
    ];
    setHistory(next);
    setDraft("");
    setStreaming(true);
    if (taRef.current) taRef.current.style.height = "auto";
    persist(next); // keep the question even if the user tabs away mid-stream
    armSilence();
    try {
      await api.curatorSend(
        base,
        message,
        metaRef.current.phase,
        metaRef.current.activeId,
      );
    } catch (e) {
      clearSilence();
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

  function switchThread(id: string) {
    if (id === activeId) return;
    const t = threads.find((x) => x.id === id);
    if (t) loadThread(t);
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

  // The active thread may be a brand-new unsaved one; make sure it shows.
  const threadOpts: Opt[] = [
    ...(threads.some((t) => t.id === activeId)
      ? []
      : [{ value: activeId, label: title || "New chat" }]),
    ...threads.map((t) => ({
      value: t.id,
      label: (t.title || "New chat") + (t.instance_id ? "  ·  pack" : ""),
    })),
  ];

  return (
    <div className="chat">
      <div className="chat-threadbar">
        <Dropdown
          value={activeId}
          options={threadOpts}
          onChange={switchThread}
        />
        {boundInstance && (
          <button
            className="thread-tag"
            title="Open this pack in Instances"
            onClick={() => onNavigate("instances")}
          >
            ▦ {title || "pack"}
          </button>
        )}
        <span className={"phase-chip phase-" + phase} title="Pipeline phase">
          {phase}
        </span>
        <button
          className="btn ghost thread-new"
          onClick={newThread}
          disabled={empty && !boundInstance}
        >
          + New chat
        </button>
      </div>

      <div className="chat-scroll" ref={scrollRef}>
        {empty ? (
          <div className="chat-empty">
            <div className="chat-empty-mark" aria-hidden>
              ✦
            </div>
            <h2 className="chat-empty-title">Design a modpack</h2>
            <p className="chat-empty-sub">
              Describe what you want to play. Anvil finds the mods, resolves
              dependencies, and builds a launchable pack.
            </p>
            <div className="chat-empty-grid">
              {EXAMPLES.map((ex) => (
                <button
                  key={ex.text}
                  className="ex-card"
                  data-tag={ex.tag}
                  onClick={() => applyExample(ex.text)}
                >
                  {ex.text}
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
                <div key={i}>
                  {m.content && (
                    <div className="chat-turn-label">Anvil</div>
                  )}
                  {m.content ? (
                    <div
                      className="chat-msg-assistant md"
                      onClick={openExternal}
                      dangerouslySetInnerHTML={{
                        __html: renderMd(m.content),
                      }}
                    />
                  ) : (
                    <div className="chat-msg-assistant">
                      {streaming &&
                        i === history.length - 1 &&
                        !activity && <span className="caret" />}
                    </div>
                  )}
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
        <div className="composer-hint">
          <kbd>Enter</kbd> to send, <kbd>Shift</kbd>+<kbd>Enter</kbd> for a new
          line
        </div>
      </div>
    </div>
  );
}
