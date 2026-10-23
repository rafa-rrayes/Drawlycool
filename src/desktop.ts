import { Channel, invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { clientColor } from "./colors";

export type CanvasMeta = {
  id: string;
  name: string;
  createdAt: number;
  updatedAt: number;
  /** The folder it's filed in; absent at the top level. */
  folder?: string;
};

/** A named group of canvases in one space (one level, no nesting). */
export type Folder = { id: string; name: string; createdAt: number };

export type Viewport = { scrollX: number; scrollY: number; zoom: number };

export type ThemePreference = "light" | "dark" | "system";

/** How you appear to other people on shared canvases. `id` is stable and picks your colour. */
export type Profile = { name: string; id: string };

export type Settings = {
  theme?: ThemePreference;
  profile?: Profile;
  /** Card order on the Welcome screen, per space ("local" or a server id), and per folder ("<space>/<folder>"). */
  canvasOrder?: Record<string, string[]>;
};

export type MenuAction = "new-canvas" | "go-home" | "import" | "save" | "connect" | "host";

type ImportResult = { imported: CanvasMeta[]; errors: string[] };

export type ServerInfo = { id: string; name: string; url: string; isHost: boolean };

/** A server joined before and since removed, which can be rejoined in one click. */
export type RecentServer = { url: string; name: string; lastConnected: number };

/** Someone in a canvas: `id` is their connection, `userId` the person. */
export type Peer = { id: string; name: string; color: string; userId: string };

export type RemoteCanvas = CanvasMeta & { peers: Peer[] };

export type HostAddress = { label: string; address: string };

/** The Cloudflare tunnel behind "Anyone, over the internet". */
export type TunnelPhase =
  | { state: "off" | "missing" | "starting" }
  | { state: "ready"; url: string }
  | { state: "failed"; error: string };

export type HostStatus = {
  enabled: boolean;
  running: boolean;
  port: number;
  name: string;
  inviteAddress: string | null;
  /** Empty while the internet link is on its way. */
  inviteUrl: string;
  addresses: HostAddress[];
  error: string | null;
  internet: boolean;
  tunnel: TunnelPhase;
  /** Terminal command that installs cloudflared. */
  installCommand: string;
};

export type HostPatch = Partial<{
  enabled: boolean;
  port: number;
  name: string;
  inviteAddress: string;
  internet: boolean;
  resetToken: boolean;
}>;

/** Ready-to-paste settings for one address of a server's `/mcp` endpoint. */
export type AiSnippets = {
  /** `<base>/mcp[?canvas=…]`, used with an `Authorization` header. */
  url: string;
  /** `claude mcp add …` for Terminal. */
  claudeCode: string;
  /** `{"mcpServers": …}` for apps that speak HTTP MCP. */
  jsonConfig: string;
  /** The URL with the token in it, for apps where you can only paste a URL. */
  connectorUrl: string;
  /** `{"mcpServers": …}` running `npx mcp-remote`, for apps that only start local servers. */
  bridgeConfig: string;
};

/** How an AI assistant joins a server (or one canvas on it) over MCP. */
export type AiConnectInfo = {
  serverName: string;
  canvasId: string | null;
  canvasName: string | null;
  token: string;
  /** Name the assistant's settings use for this server, e.g. `drawlycool-roadmap`. */
  mcpName: string;
  /** For assistants on this Mac (hosted servers only). */
  localUrl: string | null;
  /** For other machines and cloud apps; null while the internet link is on its way. */
  remoteUrl: string | null;
  local: AiSnippets | null;
  remote: AiSnippets | null;
  /** Why cloud apps such as claude.ai can't connect (yet). */
  cloudNote: string | null;
  /** The internet link may show up soon, so it's worth asking again. */
  waiting: boolean;
};

/** What a live session reports: connection changes, or a server message. */
export type SessionEvent =
  | { status: { state: "connecting" | "online" | "offline" | "closed"; error: string | null; fatal: boolean } }
  | { msg: ServerMessage };

export type ServerMessage =
  | { type: "init"; you: string; meta: CanvasMeta; elements: unknown[]; files: Record<string, unknown>; background: string | null; peers: Peer[] }
  | { type: "update"; from: string; elements: unknown[] }
  | { type: "files"; files: unknown[] }
  | { type: "background"; color: string }
  | { type: "pointer"; from: string; pointer: { x: number; y: number; tool: "pointer" | "laser" }; button: "up" | "down"; selectedElementIds: Record<string, true> | null }
  | { type: "peer-joined"; peer: Peer }
  | { type: "peer-left"; id: string }
  | { type: "meta"; meta: CanvasMeta }
  | { type: "followed-by"; ids: string[] }
  | { type: "bounds"; from: string; bounds: [number, number, number, number] }
  | { type: "deleted" };

export type ClientMessage =
  | { type: "update"; elements: readonly unknown[] }
  | { type: "files"; files: unknown[] }
  | { type: "background"; color: string }
  | { type: "pointer"; pointer: unknown; button: string; selectedElementIds: unknown }
  | { type: "follow"; target: string; follow: boolean }
  | { type: "bounds"; bounds: readonly number[] };

export type Session = {
  send: (msg: ClientMessage) => Promise<void>;
  close: () => Promise<void>;
};

const encoder = new TextEncoder();
const decoder = new TextDecoder();

export const desktop = {
  list: () => invoke<CanvasMeta[]>("list_canvases"),
  create: (name: string, folder?: string) => invoke<CanvasMeta>("create_canvas", { name, folder: folder ?? null }),

  async load(id: string): Promise<{ meta: CanvasMeta; scene: string; viewport: Viewport | null }> {
    const [{ meta, viewport }, bytes] = await Promise.all([
      invoke<{ meta: CanvasMeta; viewport: Viewport | null }>("canvas_meta", { id }),
      invoke<ArrayBuffer>("load_scene", { id }),
    ]);
    return { meta, viewport, scene: decoder.decode(bytes) };
  },

  saveScene: (id: string, json: string) =>
    invoke<void>("save_scene", encoder.encode(json), { headers: { "x-canvas-id": id } }),
  saveViewport: (id: string, viewport: Viewport) => invoke<void>("save_viewport", { id, viewport }),
  rename: (id: string, name: string) => invoke<CanvasMeta>("rename_canvas", { id, name }),
  duplicate: (id: string) => invoke<CanvasMeta>("duplicate_canvas", { id }),
  remove: (id: string) => invoke<boolean>("delete_canvas", { id }),
  /** Files a canvas in a folder, or takes it out (null). */
  setFolder: (id: string, folder: string | null) => invoke<CanvasMeta>("set_canvas_folder", { id, folder }),

  folders: {
    list: () => invoke<Folder[]>("list_folders"),
    create: (name: string) => invoke<Folder>("create_folder", { name }),
    rename: (id: string, name: string) => invoke<Folder>("rename_folder", { id, name }),
    /** Its canvases move to the top level. */
    remove: (id: string) => invoke<void>("delete_folder", { id }),
  },
  reveal: (id: string) => invoke<void>("reveal_canvas", { id }),
  exportCanvas: (id: string) => invoke<boolean>("export_canvas", { id }),
  importDialog: () => invoke<ImportResult>("import_dialog"),
  importData: (files: { name: string; text: string }[]) => invoke<ImportResult>("import_data", { files }),
  getSettings: () => invoke<Settings>("get_settings"),
  setSettings: (patch: Partial<Settings>) => invoke<Settings>("set_settings", { patch }),

  saveDownload: (name: string, bytes: Uint8Array) =>
    invoke<boolean>("save_download", bytes, { headers: { "x-file-name": encodeURIComponent(name) } }),
  openUrl: (url: string) => invoke<void>("open_url", { url }),
  copyText: (text: string) => invoke<void>("copy_text", { text }),
  userFullName: () => invoke<string>("user_full_name"),

  /**
   * Moves a canvas between this Mac (`null`) and servers, into `folder` there
   * if given; the original goes once the copy has arrived. Resolves with the copy.
   */
  move: (canvasId: string, name: string, from: string | null, to: string | null, folder?: string) =>
    invoke<CanvasMeta>("move_canvas", { canvasId, name, from, to, folder: folder ?? null }),

  servers: {
    list: () => invoke<ServerInfo[]>("list_servers"),
    add: (url: string, token: string) => invoke<ServerInfo>("add_server", { url, token }),
    remove: (id: string) => invoke<void>("remove_server", { id }),
    recent: () => invoke<RecentServer[]>("list_recent_servers"),
    rejoin: (url: string) => invoke<ServerInfo>("reconnect_server", { url }),
    forget: (url: string) => invoke<void>("forget_server", { url }),
    inviteLink: (serverId: string, canvasId?: string) =>
      invoke<string>("invite_link", { serverId, canvasId: canvasId ?? null }),
    /** Settings for connecting an AI assistant over MCP, starting in a canvas if given. */
    aiConnectInfo: (serverId: string, canvasId?: string, canvasName?: string) =>
      invoke<AiConnectInfo>("ai_connect_info", { serverId, canvasId: canvasId ?? null, canvasName: canvasName ?? null }),
    takePendingLinks: () => invoke<string[]>("take_pending_links"),
    /** A `drawlycool://` link was opened (e.g. clicked in a chat). */
    onLinkOpened(callback: () => void) {
      const unlisten = listen("open-link", () => callback());
      return () => void unlisten.then((f) => f());
    },
  },

  remote: {
    list: (serverId: string) => invoke<RemoteCanvas[]>("remote_list", { serverId }),
    create: (serverId: string, name: string, folder?: string) =>
      invoke<RemoteCanvas>("remote_create", { serverId, name, folder: folder ?? null }),
    rename: (serverId: string, canvasId: string, name: string) =>
      invoke<RemoteCanvas>("remote_rename", { serverId, canvasId, name }),
    duplicate: (serverId: string, canvasId: string) =>
      invoke<RemoteCanvas>("remote_duplicate", { serverId, canvasId }),
    remove: (serverId: string, canvasId: string) => invoke<boolean>("remote_delete", { serverId, canvasId }),
    setFolder: (serverId: string, canvasId: string, folder: string | null) =>
      invoke<RemoteCanvas>("remote_set_folder", { serverId, canvasId, folder }),
    folders: {
      /** Null for servers from before folders existed. */
      list: (serverId: string) => invoke<Folder[] | null>("remote_folders", { serverId }),
      create: (serverId: string, name: string) => invoke<Folder>("remote_create_folder", { serverId, name }),
      rename: (serverId: string, folderId: string, name: string) =>
        invoke<Folder>("remote_rename_folder", { serverId, folderId, name }),
      remove: (serverId: string, folderId: string) => invoke<void>("remote_delete_folder", { serverId, folderId }),
    },
    exportCanvas: (serverId: string, canvasId: string, name: string) =>
      invoke<boolean>("remote_export", { serverId, canvasId, name }),

    async connect(serverId: string, canvasId: string, profile: Profile, onEvent: (e: SessionEvent) => void): Promise<Session> {
      const channel = new Channel<SessionEvent>(onEvent);
      const session = await invoke<number>("remote_connect", {
        serverId,
        canvasId,
        name: profile.name,
        color: clientColor(profile.id),
        userId: profile.id,
        onMessage: channel,
      });
      return {
        send: (msg) =>
          invoke<void>("remote_send", encoder.encode(JSON.stringify(msg)), { headers: { "x-session": String(session) } }),
        close: () => invoke<void>("remote_disconnect", { session }),
      };
    },
  },

  host: {
    status: () => invoke<HostStatus>("host_status"),
    update: (patch: HostPatch) => invoke<HostStatus>("host_update", { patch }),
    /** Canvases (and folders) shared from this Mac, while sharing is off. */
    paused: () => invoke<{ canvases: CanvasMeta[]; folders: Folder[] }>("paused_canvases"),
  },

  onMenu(callback: (message: { action: MenuAction }) => void) {
    const unlisten = listen<MenuAction>("menu", (e) => callback({ action: e.payload }));
    return () => void unlisten.then((f) => f());
  },

  /** The app asks for pending saves to be flushed right before quitting. */
  onFlushRequest(callback: () => Promise<void> | void) {
    const unlisten = listen("app:flush", async () => {
      try {
        await callback();
      } finally {
        invoke("flushed");
      }
    });
    return () => void unlisten.then((f) => f());
  },
};

/**
 * Parses an invite link: `drawlycool://host[:port]/token[/canvas]` (a port
 * means http, no port https) or `drawlycool://connect?server=…&token=…&canvas=…`.
 */
export function parseInvite(link: string): { server: string; token: string; canvas: string | null } | null {
  try {
    const url = new URL(link.trim());
    if (url.protocol !== "drawlycool:") return null;
    const server = url.searchParams.get("server");
    if (server) {
      const token = url.searchParams.get("token");
      return token ? { server, token, canvas: url.searchParams.get("canvas") } : null;
    }
    const [token, canvas] = url.pathname.split("/").filter(Boolean).map(decodeURIComponent);
    if (!url.host || !token) return null;
    return { server: `${url.port ? "http" : "https"}://${url.host}`, token, canvas: canvas ?? null };
  } catch {
    return null;
  }
}
