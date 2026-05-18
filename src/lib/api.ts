import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

export interface SearchHit {
  project_id: string;
  slug: string;
  title: string;
  description: string;
  categories: string[];
  client_side: string;
  server_side: string;
  project_type: string;
  downloads: number;
  follows: number;
  icon_url: string | null;
  author: string;
  versions: string[];
  display_categories: string[];
  license: string | null;
}

export interface SearchResponse {
  hits: SearchHit[];
  offset: number;
  limit: number;
  total_hits: number;
}

export interface GalleryItem {
  url: string;
  featured: boolean;
  title: string | null;
  description: string | null;
}

export interface Project {
  id: string;
  slug: string;
  title: string;
  description: string;
  body: string;
  categories: string[];
  client_side: string;
  server_side: string;
  project_type: string;
  downloads: number;
  followers: number;
  icon_url: string | null;
  gallery: GalleryItem[];
  license: { id: string; name: string; url: string | null };
  source_url: string | null;
  issues_url: string | null;
  wiki_url: string | null;
  game_versions: string[];
  loaders: string[];
}

export interface Version {
  id: string;
  project_id: string;
  name: string;
  version_number: string;
  game_versions: string[];
  version_type: string;
  loaders: string[];
  downloads: number;
  date_published: string;
}

export interface ModDetail {
  project: Project;
  versions: Version[];
}

export interface AppInfo {
  name: string;
  version: string;
  status?: string;
}

export interface InstanceMod {
  project_id: string;
  version_id: string;
  name: string;
  path: string;
  sha1: string;
  sha512: string;
  download_url: string;
  file_size: number;
}

export interface Instance {
  id: string;
  name: string;
  mc_version: string;
  loader: string;
  loader_version: string;
  created: string;
  last_played: string | null;
  mods: InstanceMod[];
}

export interface UpdateInfo {
  project_id: string;
  name: string;
  from: string;
  to: string;
}

export interface Keybind {
  name: string;
  label: string;
  key_token: string;
  key_display: string;
  conflict: boolean;
}

export interface KeyGroup {
  mod_name: string;
  binds: Keybind[];
}

export interface KeybindReport {
  /** False when the instance has no options.txt (never launched). */
  launched: boolean;
  groups: KeyGroup[];
  conflict_count: number;
}

export interface QuestTask {
  [k: string]: unknown;
}

export interface QuestReward {
  [k: string]: unknown;
}

/** A custom recipe carried by a quest node (Slice 2 facet). Open bag like
 *  QuestTask/QuestReward: the curator emits shaped/shapeless/smelting shapes
 *  and the datapack id is derived server-side, so the UI never types it
 *  strictly — it only reads `type` and the `result` for display. */
export interface QuestRecipe {
  [k: string]: unknown;
}

/** A provisioned-content facet on a quest node (Slice 3). Open bag like the
 *  recipe facet: the Rust side owns the tagged-union shape and derives every
 *  datapack id, so the UI only reads `kind`, `entity` and the token names for
 *  the "Boss" badge + encounter summary. */
export interface QuestContent {
  kind?: string;
  entity?: string;
  display_name?: string;
  token_item?: string;
  token_name?: string;
  [k: string]: unknown;
}

export interface Quest {
  id: string;
  title: string;
  description: string;
  x: number;
  y: number;
  deps: string[];
  tasks: QuestTask[];
  rewards: QuestReward[];
  /** Optional recipe facet; absent on plain quest nodes (serde default). */
  recipes?: QuestRecipe[];
  /** Optional content facet (Slice 3 provisioned boss); absent on plain
   *  quest/recipe nodes (serde default = null). */
  content?: QuestContent | null;
}

export interface QuestChapter {
  id: string;
  title: string;
  quests: Quest[];
}

export interface QuestGraph {
  title: string;
  chapters: QuestChapter[];
}

export interface QuestIssue {
  kind: string;
  [k: string]: unknown;
}

export interface OriginPower {
  name: string;
  description: string;
  shipped: boolean;
}

export interface OriginEntry {
  id: string;
  name: string;
  description: string;
  icon: string;
  impact: number;
  powers: OriginPower[];
}

export interface OriginsView {
  origins: OriginEntry[];
}

export type ThemePref = "light" | "dark" | "system";

export interface Settings {
  has_anthropic_key: boolean;
  ms_client_id: string | null;
  theme: ThemePref;
}

export interface AuthAccount {
  username: string;
  uuid: string;
}

export type ChatRole = "user" | "assistant";

export interface ChatMessage {
  role: ChatRole;
  content: string;
}

export type Phase = "curating" | "assembled" | "progression" | "complete";

export interface ChatThread {
  id: string;
  /** Set once the curator assembled a pack from this thread. */
  instance_id: string | null;
  title: string;
  created: string;
  updated: string;
  /** Pipeline phase; scopes the curator's prompt + tools. */
  phase: Phase;
  messages: ChatMessage[];
}

/* Adjacently tagged on the Rust side: { "kind": "...", "data": payload }.
   Struct variants carry an object in `data`; newtype variants a scalar. */
export type CuratorEvent =
  | { kind: "text"; data: string }
  | { kind: "tool"; data: { name: string; status: string } }
  | { kind: "assembled"; data: { instance_id: string; name: string } }
  | { kind: "phase"; data: string }
  | {
      kind: "usage";
      data: {
        input_tokens: number;
        cache_creation_input_tokens: number;
        cache_read_input_tokens: number;
        output_tokens: number;
      };
    }
  | { kind: "done" }
  | { kind: "error"; data: string };

export type LaunchEvent =
  | { kind: "status"; data: string }
  | { kind: "progress"; data: { done: number; total: number; what: string } }
  | { kind: "log"; data: string }
  | { kind: "exited"; data: number }
  | { kind: "error"; data: string };

/** Tier 3 smoke-test verdict (adjacently tagged on the Rust side). */
export type SmokeVerdict =
  | { kind: "ok" }
  | { kind: "failed"; data: { mod_name: string | null; reason: string } }
  | { kind: "inconclusive"; data: { reason: string } };

export type AuthEvent =
  | { status: "signed_in"; username: string }
  | { error: string };

/** Rejection value (Tauri-serialized `Err`) from add/remove mod commands.
 *  A caught error from `invoke` may be one of these objects OR a plain
 *  string (older / unexpected path) — always run it through
 *  `formatEditError` before showing it. */
export type ApplyEditError =
  | { kind: "still_required"; items: { label: string; required_by: string[] }[] }
  | { kind: "conflicts"; issues: unknown[] }
  | { kind: "resolve"; message: string }
  | { kind: "not_found"; instance_id: string };

/** Turn an add/remove rejection (structured `ApplyEditError`, a plain
 *  string, or anything unexpected) into a single user-facing message.
 *  For `still_required` every blocked item is listed on its own line with
 *  the mods that still depend on it, so the user has a clear path forward
 *  (remove those too). Multi-line output relies on the renderer using
 *  `white-space: pre-line` (or `<br>`). */
export function formatEditError(e: unknown): string {
  if (typeof e === "string") return e;
  if (e && typeof e === "object" && "kind" in e) {
    const err = e as ApplyEditError;
    switch (err.kind) {
      case "still_required":
        return err.items
          .map((it) => {
            const by = it.required_by.join(", ");
            return by
              ? `${it.label} is still required by ${by} — remove those too to remove it.`
              : `${it.label} is still required by other mods — remove those too to remove it.`;
          })
          .join("\n");
      case "conflicts": {
        const n = Array.isArray(err.issues) ? err.issues.length : 0;
        return n > 0
          ? `Could not apply — ${n} dependency conflict${n === 1 ? "" : "s"} detected. Try a different version or remove the conflicting mod.`
          : "Could not apply — dependency conflicts detected. Try a different version or remove the conflicting mod.";
      }
      case "resolve":
        return err.message;
      case "not_found":
        return "Instance not found — it may have been deleted. Reopen it and try again.";
    }
  }
  return String(e);
}

/** Thin typed wrapper over Tauri's event listen. Returns the promise of an
    unlisten fn so callers can clean up in a useEffect teardown. */
export function on<T>(
  event: string,
  handler: (payload: T) => void,
): Promise<UnlistenFn> {
  return listen<T>(event, (e) => handler(e.payload));
}

export const api = {
  searchMods: (opts: {
    query?: string;
    mcVersion?: string;
    loader?: string;
    projectType?: string;
    categories?: string[];
    index?: string;
    offset?: number;
  }) =>
    invoke<SearchResponse>("search_mods", {
      query: opts.query ?? "",
      mcVersion: opts.mcVersion || null,
      loader: opts.loader || null,
      projectType: opts.projectType || "mod",
      categories: opts.categories && opts.categories.length ? opts.categories : null,
      index: opts.index || null,
      offset: opts.offset ?? null,
    }),
  getMod: (idOrSlug: string) =>
    invoke<ModDetail>("get_mod", { idOrSlug }),
  appInfo: () => invoke<AppInfo>("app_info"),
  listInstances: () => invoke<Instance[]>("list_instances"),

  getSettings: () => invoke<Settings>("get_settings"),
  setSettings: (args: {
    anthropicApiKey?: string;
    msClientId?: string;
    theme?: ThemePref;
  }) => invoke<void>("set_settings", args),

  authStatus: () => invoke<AuthAccount | null>("auth_status"),
  authSignout: () => invoke<void>("auth_signout"),
  authStart: () => invoke<void>("auth_start"),

  curatorSend: (
    history: ChatMessage[],
    message: string,
    phase: Phase,
    threadId: string,
  ) => invoke<void>("curator_send", { history, message, phase, threadId }),

  listChats: () => invoke<ChatThread[]>("list_chats"),
  getChat: (threadId: string) =>
    invoke<ChatThread | null>("get_chat", { threadId }),
  saveChat: (thread: ChatThread) => invoke<void>("save_chat", { thread }),
  deleteChat: (threadId: string) =>
    invoke<void>("delete_chat", { threadId }),
  chatForInstance: (instanceId: string) =>
    invoke<ChatThread | null>("chat_for_instance", { instanceId }),

  launchInstance: (instanceId: string) =>
    invoke<void>("launch_instance", { instanceId }),
  smokeTestInstance: (instanceId: string) =>
    invoke<SmokeVerdict>("smoke_test_instance", { instanceId }),
  importMrpack: (path: string) =>
    invoke<Instance>("import_mrpack", { path }),

  createInstance: (args: {
    name: string;
    mcVersion: string;
    loader: string;
    loaderVersion: string;
  }) => invoke<Instance>("create_instance", args),
  deleteInstance: (instanceId: string) =>
    invoke<void>("delete_instance", { instanceId }),
  duplicateInstance: (instanceId: string, newName: string) =>
    invoke<Instance>("duplicate_instance", { instanceId, newName }),

  addModToInstance: (
    instanceId: string,
    projectId: string,
    versionId?: string,
  ) =>
    invoke<Instance>("add_mod_to_instance", { instanceId, projectId, versionId }),
  removeModFromInstance: (instanceId: string, projectId: string) =>
    invoke<Instance>("remove_mod_from_instance", { instanceId, projectId }),

  getKeybinds: (instanceId: string) =>
    invoke<KeybindReport>("get_keybinds", { instanceId }),
  setKeybinds: (
    instanceId: string,
    changes: { name: string; token: string }[],
  ) => invoke<void>("set_keybinds", { instanceId, changes }),

  checkInstanceUpdates: (instanceId: string) =>
    invoke<UpdateInfo[]>("check_instance_updates", { instanceId }),
  applyInstanceUpdates: (instanceId: string, projectIds: string[]) =>
    invoke<Instance>("apply_instance_updates", { instanceId, projectIds }),

  getQuestGraph: (instanceId: string) =>
    invoke<QuestGraph | null>("get_quest_graph", { instanceId }),
  saveQuestGraph: (instanceId: string, graph: QuestGraph) =>
    invoke<void>("save_quest_graph", { instanceId, graph }),
  validateQuestGraph: (instanceId: string, graph: QuestGraph) =>
    invoke<QuestIssue[]>("validate_quest_graph", { instanceId, graph }),
  /** A `data:image/png;base64,...` URL for an item, or null if the icon
   *  can't be resolved (vanilla without downloaded assets, 3D/builtin
   *  model, or not present in any pinned jar) — caller renders a labeled
   *  slot instead. Backed by an on-disk cache. */
  getItemIcon: (instanceId: string, itemId: string) =>
    invoke<string | null>("get_item_icon", { instanceId, itemId }),
  /** The Anvil-curated origins for this instance, already sorted by impact
   *  server-side. `null` ⟺ the instance has no Anvil origins — the Origins
   *  tab must not appear at all. Non-null always carries ≥1 origin. */
  getOrigins: (instanceId: string) =>
    invoke<OriginsView | null>("get_origins", { instanceId }),
};
