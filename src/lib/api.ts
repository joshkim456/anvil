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

export interface Instance {
  id: string;
  name: string;
  mc_version: string;
  loader: string;
  loader_version: string;
  created: string;
  last_played: string | null;
  mods: { project_id: string; version_id: string; name: string }[];
}

export interface Settings {
  has_anthropic_key: boolean;
  ms_client_id: string | null;
}

export interface AuthAccount {
  username: string;
  uuid: string;
}

export interface DeviceCode {
  device_code: string;
  user_code: string;
  verification_uri: string;
  interval: number;
  expires_in: number;
}

export type ChatRole = "user" | "assistant";

export interface ChatMessage {
  role: ChatRole;
  content: string;
}

/* Tauri tuple enum variants serialize as { "kind": "...", "0": value }.
   Access the payload via evt["0"]. */
export type CuratorEvent =
  | { kind: "text"; "0": string }
  | { kind: "tool"; name: string; status: string }
  | { kind: "assembled"; instance_id: string; name: string }
  | { kind: "done" }
  | { kind: "error"; "0": string };

export type LaunchEvent =
  | { kind: "status"; "0": string }
  | { kind: "progress"; done: number; total: number; what: string }
  | { kind: "log"; "0": string }
  | { kind: "exited"; "0": number }
  | { kind: "error"; "0": string };

export type AuthEvent =
  | { status: "signed_in"; username: string }
  | { error: string };

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
  setSettings: (args: { anthropicApiKey?: string; msClientId?: string }) =>
    invoke<void>("set_settings", args),

  authStatus: () => invoke<AuthAccount | null>("auth_status"),
  authSignout: () => invoke<void>("auth_signout"),
  authStart: () => invoke<DeviceCode>("auth_start"),

  curatorSend: (history: ChatMessage[], message: string) =>
    invoke<void>("curator_send", { history, message }),

  launchInstance: (instanceId: string) =>
    invoke<void>("launch_instance", { instanceId }),
  importMrpack: (path: string) =>
    invoke<Instance>("import_mrpack", { path }),
};
