import { Channel, invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { Menu } from "@tauri-apps/api/menu";
import { clientColor } from "./colors";

export type CanvasMeta = {
  id: string;
  name: string;
  createdAt: number;
  updatedAt: number;
};

export type Viewport = { scrollX: number; scrollY: number; zoom: number };

export type ThemePreference = "light" | "dark" | "system";

/** How you appear to other people on shared canvases. `id` is stable and picks your colour. */
export type Profile = { name: string; id: string };

export type Settings = { theme?: ThemePreference; profile?: Profile };

export type MenuAction = "new-canvas" | "go-home" | "import" | "save" | "connect" | "host";

type ImportResult = { imported: CanvasMeta[]; errors: string[] };

export type ServerInfo = { id: string; name: string; url: string; isHost: boolean };

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

/** One entry of a native context menu. */
export type MenuEntry =
  | { id: string; text: string; enabled?: boolean }
  | { text: string; items: MenuEntry[]; enabled?: boolean }
  | "separator";

const encoder = new TextEncoder();
const decoder = new TextDecoder();

/** Shows a native context menu and resolves with the picked item's id. */
async function contextMenu(entries: MenuEntry[]): Promise<string | null> {
  let choice: string | null = null;
  let picked: () => void = () => {};
  const pickedPromise = new Promise<void>((resolve) => (picked = resolve));
  type Built = Parameters<typeof Menu.new>[0] extends { items?: (infer I)[] } | undefined ? I : never;
  const build = (entry: MenuEntry): Built => {
    if (entry === "separator") return { item: "Separator" } as Built;
    if ("items" in entry) return { text: entry.text, enabled: entry.enabled ?? true, items: entry.items.map(build) } as Built;
    return {
      id: entry.id,
      text: entry.text,
      enabled: entry.enabled ?? true,
      action: () => {
        choice = entry.id;
        picked();
      },
    } as Built;
  };
  const menu = await Menu.new({ items: entries.map(build) });
  await menu.popup();
  // popup() returns once the menu closes; the click event may land a tick later.
  await Promise.race([pickedPromise, new Promise((r) => setTimeout(r, 150))]);
  menu.close();
  return choice;
}

export const desktop = {
  list: () => invoke<CanvasMeta[]>("list_canvases"),
  create: (name: string) => invoke<CanvasMeta>("create_canvas", { name }),

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
  reveal: (id: string) => invoke<void>("reveal_canvas", { id }),
  exportCanvas: (id: string) => invoke<boolean>("export_canvas", { id }),
  importDialog: () => invoke<ImportResult>("import_dialog"),
  importData: (files: { name: string; text: string }[]) => invoke<ImportResult>("import_data", { files }),
  contextMenu,
  getSettings: () => invoke<Settings>("get_settings"),
  setSettings: (patch: Partial<Settings>) => invoke<Settings>("set_settings", { patch }),

  saveDownload: (name: string, bytes: Uint8Array) =>
    invoke<boolean>("save_download", bytes, { headers: { "x-file-name": encodeURIComponent(name) } }),
  openUrl: (url: string) => invoke<void>("open_url", { url }),
  showError: (title: string, message: string) => invoke<void>("show_error", { title, message }),
  copyText: (text: string) => invoke<void>("copy_text", { text }),
  userFullName: () => invoke<string>("user_full_name"),

  servers: {
    list: () => invoke<ServerInfo[]>("list_servers"),
    add: (url: string, token: string) => invoke<ServerInfo>("add_server", { url, token }),
    remove: (id: string) => invoke<void>("remove_server", { id }),
    inviteLink: (serverId: string, canvasId?: string) =>
      invoke<string>("invite_link", { serverId, canvasId: canvasId ?? null }),
    takePendingLinks: () => invoke<string[]>("take_pending_links"),
    /** A `drawlycool://` link was opened (e.g. clicked in a chat). */
    onLinkOpened(callback: () => void) {
      const unlisten = listen("open-link", () => callback());
      return () => void unlisten.then((f) => f());
    },
  },

  remote: {
    list: (serverId: string) => invoke<RemoteCanvas[]>("remote_list", { serverId }),
    create: (serverId: string, name: string) => invoke<RemoteCanvas>("remote_create", { serverId, name }),
    rename: (serverId: string, canvasId: string, name: string) =>
      invoke<RemoteCanvas>("remote_rename", { serverId, canvasId, name }),
    duplicate: (serverId: string, canvasId: string) =>
      invoke<RemoteCanvas>("remote_duplicate", { serverId, canvasId }),
    remove: (serverId: string, canvasId: string, name: string) =>
      invoke<boolean>("remote_delete", { serverId, canvasId, name }),
    exportCanvas: (serverId: string, canvasId: string, name: string) =>
      invoke<boolean>("remote_export", { serverId, canvasId, name }),
    /** Uploads a local canvas; resolves with the remote copy, or null if cancelled. */
    moveToServer: (canvasId: string, serverId: string) =>
      invoke<RemoteCanvas | null>("move_to_server", { canvasId, serverId }),

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
