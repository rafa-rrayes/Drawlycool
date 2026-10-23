import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
  type CSSProperties,
  type DragEvent,
  type MouseEvent,
  type PointerEvent,
  type ReactNode,
  type RefObject,
} from "react";
import {
  desktop,
  parseInvite,
  type CanvasMeta,
  type Folder,
  type HostStatus,
  type Peer,
  type Profile,
  type ServerInfo,
} from "./desktop";
import { RoughBox, seedFrom } from "./RoughBox";
import { AiConnectPanel, ConnectPanel, HostPanel, ProfilePanel, type Panel } from "./Panels";
import { ui, anchorOf, type Anchor, type MenuEntry } from "./ui";
import { clientColor, initials } from "./colors";
import { DrawnIcon, type IconName } from "./icons";
import { edgeSpeed, useCardDrag, type CardDrag, type DropTarget } from "./cardDrag";

const relative = new Intl.RelativeTimeFormat(undefined, { numeric: "auto" });
const UNITS: [Intl.RelativeTimeFormatUnit, number][] = [
  ["year", 365 * 24 * 3600],
  ["month", 30 * 24 * 3600],
  ["week", 7 * 24 * 3600],
  ["day", 24 * 3600],
  ["hour", 3600],
  ["minute", 60],
];

function timeAgo(ts: number) {
  const seconds = (ts - Date.now()) / 1000;
  for (const [unit, size] of UNITS) {
    if (Math.abs(seconds) >= size) return relative.format(Math.round(seconds / size), unit);
  }
  return "just now";
}

const LIST_POLL = 4000;
const HOST_POLL = 2500;
/** How long a card that just moved keeps wiggling in its new place. */
const LANDED_FOR = 1600;
/** How far the pointer moves before a press on the background starts a selection box. */
const MARQUEE_THRESHOLD = 4;

/** Colours a card can be painted: CSS has a light and a dark shade of each (`--card-<id>`). */
const COLORS = [
  ["red", "Red"],
  ["orange", "Orange"],
  ["yellow", "Yellow"],
  ["green", "Green"],
  ["teal", "Teal"],
  ["blue", "Blue"],
  ["violet", "Violet"],
  ["pink", "Pink"],
] as const;
const colorVar = (color: string) => `var(--card-${color})`;

/** Space ids: canvases on this Mac, the ones shared from it, or a joined server's id. */
const LOCAL = "local";
const HOST = "this-mac";

type Card = CanvasMeta & { peers?: Peer[] };

/** A place canvases live, shown as one section of the Welcome screen. */
type Space = {
  id: string;
  title: string;
  icon: IconName;
  /** Null for canvases on this Mac, and for shared ones while sharing is off. */
  server: ServerInfo | null;
  /** All of them, in every folder. Null while loading. */
  canvases: Card[] | null;
  /** Sorted by name. Null while loading, or on servers from before folders. */
  folders: Folder[] | null;
  error: string | null;
  /** Shared from this Mac, but sharing is off. */
  paused: boolean;
  /** The folder being looked into, if any. */
  folder: Folder | null;
  /** The canvases on show (the open folder's, or the ones in no folder), in the user's order. */
  shown: Card[] | null;
};

/** Selected cards (canvases and folders), all in one space. */
type Selection = { space: string; ids: string[] };

type Box = { left: number; top: number; width: number; height: number };

type RemoteList = { canvases: Card[] | null; folders: Folder[] | null; error: string | null };

/** Cards copied with ⌘C: where from, and the text put on the clipboard, to recognise them by when pasting. */
type Clip = { space: string; canvases: Card[]; folders: { folder: Folder; canvases: Card[] }[]; text: string };

/** What was last copied. Outlives this screen, so it can be pasted after opening a canvas and coming back. */
let clip: Clip | null = null;

/** The folder open in each space. Outlives this screen, so coming back from a canvas lands in it again. */
const openFolders: Record<string, string | undefined> = {};

/** The folder a canvas shows up in: none where there are no folders, or if its folder is gone. */
function folderOf(folders: Folder[] | null, canvas: Card) {
  return canvas.folder && folders?.some((f) => f.id === canvas.folder) ? canvas.folder : null;
}

/** Key for the card order of a space's top level, or of a folder in it. */
const orderKey = (spaceId: string, folder: Folder | null) => (folder ? `${spaceId}/${folder.id}` : spaceId);

const byName = (a: Folder, b: Folder) => a.name.localeCompare(b.name, undefined, { numeric: true, sensitivity: "base" });

const message = (err: unknown) => String((err as Error)?.message ?? err);

const plural = (n: number, one: string, many: string) => `${n} ${n === 1 ? one : many}`;

/** "3 canvases and 1 folder". */
const describe = (canvases: number, folders: number) =>
  [canvases && plural(canvases, "canvas", "canvases"), folders && plural(folders, "folder", "folders")].filter(Boolean).join(" and ");

export function alertError(title: string, err: unknown) {
  ui.alert(title, message(err));
}

/** Cards in the order the user arranged them; ones not arranged yet (new) come first. */
function arrange<T extends { id: string }>(list: T[], order: string[] | undefined): T[] {
  if (!order?.length) return list;
  const rank = new Map(order.map((id, i) => [id, i]));
  const fresh = list.filter((c) => !rank.has(c.id));
  const known = list.filter((c) => rank.has(c.id)).sort((a, b) => rank.get(a.id)! - rank.get(b.id)!);
  return [...fresh, ...known];
}

type Props = {
  onOpen: (id: string) => void;
  onOpenRemote: (server: ServerInfo, canvasId: string) => void;
  /** Set by the app menu (Cmd+N) to start naming a new canvas. */
  startNewCanvas: boolean;
  onStartNewCanvasHandled: () => void;
  /** Bumped when canvases change elsewhere (e.g. menu import). */
  refreshKey: number;
  profile: Profile;
  onProfileChange: (profile: Profile) => void;
  panel: Panel | null;
  setPanel: (panel: Panel | null) => void;
};

export function Home({
  onOpen,
  onOpenRemote,
  startNewCanvas,
  onStartNewCanvasHandled,
  refreshKey,
  profile,
  onProfileChange,
  panel,
  setPanel,
}: Props) {
  const [canvases, setCanvases] = useState<CanvasMeta[] | null>(null);
  const [folders, setFolders] = useState<Folder[] | null>(null);
  const [servers, setServers] = useState<ServerInfo[]>([]);
  const [remote, setRemote] = useState<Record<string, RemoteList>>({});
  const [paused, setPaused] = useState<{ canvases: CanvasMeta[]; folders: Folder[] }>({ canvases: [], folders: [] });
  const [hostStatus, setHostStatus] = useState<HostStatus | null>(null);
  const [order, setOrder] = useState<Record<string, string[]>>({});
  const [creatingIn, setCreatingIn] = useState<string | null>(null);
  /** Naming a new folder in a space, to file `canvas` in once it exists (from "Move to → New Folder…"). */
  const [newFolder, setNewFolder] = useState<{ space: string; canvases?: Card[] } | null>(null);
  const [openFolder, setOpenFolder] = useState(() => ({ ...openFolders }));
  /** A canvas or folder id. */
  const [renaming, setRenaming] = useState<string | null>(null);
  const [moving, setMoving] = useState<string[]>([]);
  const [landed, setLanded] = useState<string | null>(null);
  const [fileDrag, setFileDrag] = useState(false);
  const [selection, setSelection] = useState<Selection | null>(null);
  /** The selection box being drawn, on screen. */
  const [marquee, setMarquee] = useState<Box | null>(null);
  const [colors, setColors] = useState<Record<string, string>>({});
  const [, forceTick] = useState(0);
  const scroller = useRef<HTMLDivElement>(null);
  /** The card last clicked, where a shift-click's run starts. */
  const lastPicked = useRef<string | null>(null);
  const colorsRef = useRef(colors);
  const stopMarquee = useRef<(() => void) | null>(null);
  const onKey = useRef<(e: KeyboardEvent) => void>(() => {});
  const onPaste = useRef<(e: ClipboardEvent) => void>(() => {});
  /** The space last worked in: where ⌘V pastes when nothing is selected. */
  const activeSpace = useRef(LOCAL);

  const hostServer = servers.find((s) => s.isHost) ?? null;
  const joined = servers.filter((s) => !s.isHost);

  // ------------------------------------------------------------ loading

  const refreshLocal = useCallback(async () => {
    const [list, folders] = await Promise.all([desktop.list(), desktop.folders.list()]);
    setCanvases(list);
    setFolders(folders);
  }, []);

  const refreshServers = useCallback(async () => {
    const list = await desktop.servers.list();
    setServers(list);
    // Shared canvases stay visible (paused) while sharing is off.
    setPaused(list.some((s) => s.isHost) ? { canvases: [], folders: [] } : await desktop.host.paused());
    return list;
  }, []);

  const loadRemote = useCallback(async (serverId: string) => {
    try {
      const [list, folders] = await Promise.all([desktop.remote.list(serverId), desktop.remote.folders.list(serverId)]);
      setRemote((r) => ({ ...r, [serverId]: { canvases: list, folders, error: null } }));
    } catch (err) {
      setRemote((r) => ({ ...r, [serverId]: { canvases: null, folders: null, error: message(err) } }));
    }
  }, []);

  useEffect(() => {
    refreshLocal();
    refreshServers();
  }, [refreshLocal, refreshServers, refreshKey]);

  useEffect(() => {
    desktop.getSettings().then((s) => {
      setOrder(s.canvasOrder ?? {});
      colorsRef.current = s.cardColors ?? {};
      setColors(colorsRef.current);
    });
  }, []);

  useEffect(() => {
    const handler = (e: KeyboardEvent) => onKey.current(e);
    window.addEventListener("keydown", handler);
    return () => {
      window.removeEventListener("keydown", handler);
      stopMarquee.current?.();
    };
  }, []);

  const serverIds = servers.map((s) => s.id).join(",");
  useEffect(() => {
    const ids = serverIds ? serverIds.split(",") : [];
    const loadAll = () => ids.forEach(loadRemote);
    loadAll();
    const timer = setInterval(loadAll, LIST_POLL);
    return () => clearInterval(timer);
  }, [serverIds, loadRemote]);

  // Sharing's internet link takes a moment (or a one-time install): keep an eye on it.
  const hosting = hostServer !== null;
  useEffect(() => {
    if (!hosting) return setHostStatus(null);
    let live = true;
    const load = () => desktop.host.status().then((s) => live && setHostStatus(s));
    load();
    const timer = setInterval(load, HOST_POLL);
    return () => {
      live = false;
      clearInterval(timer);
    };
  }, [hosting]);

  useEffect(() => {
    // Keep "edited x minutes ago" fresh.
    const timer = setInterval(() => forceTick((n) => n + 1), 30_000);
    return () => clearInterval(timer);
  }, []);

  useEffect(() => {
    if (!startNewCanvas) return;
    setCreatingIn(LOCAL);
    onStartNewCanvasHandled();
  }, [startNewCanvas, onStartNewCanvasHandled]);

  useEffect(() => {
    if (!landed) return;
    const timer = setTimeout(() => setLanded(null), LANDED_FOR);
    return () => clearTimeout(timer);
  }, [landed]);

  useEffect(() => {
    const handler = (e: ClipboardEvent) => onPaste.current(e);
    window.addEventListener("paste", handler);
    return () => window.removeEventListener("paste", handler);
  }, []);

  // ------------------------------------------------------------ spaces

  const spaces = ([
    { id: LOCAL, title: "Just me", icon: "laptop", server: null, canvases, folders, error: null, paused: false },
    hostServer
      ? { id: HOST, title: "With friends", icon: "friends", server: hostServer, ...listOf(remote[HOST]), paused: false }
      : { id: HOST, title: "With friends", icon: "friends", server: null, ...paused, error: null, paused: true },
    ...joined.map((s) => ({ id: s.id, title: s.name, icon: "cloud" as const, server: s, ...listOf(remote[s.id]), paused: false })),
  ] satisfies Omit<Space, "folder" | "shown">[]).map((space): Space => {
    const folder = space.folders?.find((f) => f.id === openFolder[space.id]) ?? null;
    const shown = space.canvases?.filter((c) => folderOf(space.folders, c) === (folder?.id ?? null));
    return {
      ...space,
      folders: space.folders && [...space.folders].sort(byName),
      folder,
      shown: shown ? arrange(shown, order[orderKey(space.id, folder)]) : null,
    };
  });

  const spaceOf = (id: string) => spaces.find((s) => s.id === id)!;

  const reload = (spaceId: string) => (spaceId === LOCAL ? refreshLocal() : loadRemote(spaceId));

  /** Looks into a folder of a space, or back at its top level (null). */
  const enter = (spaceId: string, folderId: string | null) => {
    openFolders[spaceId] = folderId ?? undefined;
    setOpenFolder({ ...openFolders });
    setCreatingIn(null);
    setSelection(null);
  };

  const saveOrder = (key: string, ids: string[]) => {
    const next = { ...order, [key]: ids };
    setOrder(next);
    desktop.setSettings({ canvasOrder: next });
  };

  /** Puts cards at `index` among the cards a space shows, remembering the arrangement. */
  const place = (space: Space, moved: string[], index: number | null) => {
    const ids = (space.shown ?? []).map((c) => c.id).filter((id) => !moved.includes(id));
    ids.splice(Math.min(index ?? 0, ids.length), 0, ...moved);
    saveOrder(orderKey(space.id, space.folder), ids);
  };

  // ------------------------------------------------------------ colours

  const colorOf = (spaceId: string, id: string) => colors[`${spaceId}/${id}`] ?? null;

  const saveColors = (change: (all: Record<string, string>) => void) => {
    const next = { ...colorsRef.current };
    change(next);
    colorsRef.current = next;
    setColors(next);
    desktop.setSettings({ cardColors: next });
  };

  /** Paints cards of a space, or makes them plain again (null). */
  const paint = (spaceId: string, ids: string[], color: string | null) =>
    saveColors((all) => {
      for (const id of ids) {
        if (color) all[`${spaceId}/${id}`] = color;
        else delete all[`${spaceId}/${id}`];
      }
    });

  const forget = (spaceId: string, ids: string[]) => paint(spaceId, ids, null);

  const colorMenu = (): MenuEntry => ({
    text: "Color",
    items: [
      { id: "color:", text: "None", swatch: "" },
      "separator",
      ...COLORS.map(([id, name]) => ({ id: `color:${id}`, text: name, swatch: colorVar(id) })),
    ],
  });

  // ------------------------------------------------------------ selection

  /** Ids of the cards a space shows (folders, then canvases), in order. */
  const itemsOf = (space: Space) => [
    ...(space.folder ? [] : (space.folders ?? [])).map((f) => f.id),
    ...(space.shown ?? []).map((c) => c.id),
  ];

  /** The selected cards among the ones a space shows. */
  const selectedIn = (space: Space) => {
    if (selection?.space !== space.id) return [];
    const shown = new Set(itemsOf(space));
    return selection.ids.filter((id) => shown.has(id));
  };

  const picked = (space: Space) => {
    const ids = new Set(selectedIn(space));
    return {
      canvases: (space.shown ?? []).filter((c) => ids.has(c.id)),
      folders: (space.folder ? [] : (space.folders ?? [])).filter((f) => ids.has(f.id)),
    };
  };

  /** ⌘-click on a card adds it to the selection or takes it out; shift-click adds the run from the last one clicked. */
  const select = (space: Space, id: string, mods: { metaKey: boolean; shiftKey: boolean }) => {
    activeSpace.current = space.id;
    const current = selectedIn(space);
    const items = itemsOf(space);
    let ids = [id];
    if (mods.metaKey) {
      ids = current.includes(id) ? current.filter((other) => other !== id) : [...current, id];
    } else if (mods.shiftKey && lastPicked.current && items.includes(lastPicked.current)) {
      const [from, to] = [items.indexOf(lastPicked.current), items.indexOf(id)].sort((a, b) => a - b);
      ids = [...new Set([...current, ...items.slice(from, to + 1)])];
    }
    if (!mods.shiftKey) lastPicked.current = id;
    setSelection({ space: space.id, ids });
  };

  /** Dragging a card carries it, or every selected canvas if it's one of them. */
  const press = (space: Space, event: PointerEvent<HTMLElement>, id: string, canvas?: Card) => {
    if (event.button !== 0) return;
    activeSpace.current = space.id;
    const held = selectedIn(space).includes(id);
    if (canvas && !space.paused) start(event, held ? picked(space).canvases : [canvas], space.id);
  };

  /** Right-click (or "…") on a card: about the whole selection if it's in one, else about just that card. */
  const menuFor = (space: Space, id: string, anchor: Anchor, single: () => void) => {
    activeSpace.current = space.id;
    const current = selectedIn(space);
    if (current.length > 1 && current.includes(id)) return showSelectionMenu(space, anchor);
    single();
  };

  /** Dragging across the background draws a box; the cards it touches (in one space) get selected. */
  const onBackgroundDown = (event: PointerEvent<HTMLDivElement>) => {
    const el = scroller.current;
    if (!el || event.button !== 0 || stopMarquee.current) return;
    if ((event.target as HTMLElement).closest("button, input, textarea, a, [role=button], [data-select]")) return;
    const frame = el.getBoundingClientRect();
    if (event.clientX > frame.left + el.clientLeft + el.clientWidth) return; // on the scrollbar
    // Where it started, in the scrolled content, so it stays put while scrolling.
    const startX = event.clientX - frame.left + el.scrollLeft;
    const startY = event.clientY - frame.top + el.scrollTop;
    const startSpace = (event.target as HTMLElement).closest<HTMLElement>("[data-drop=space]")?.dataset.space;
    if (startSpace) activeSpace.current = startSpace;
    const base = event.metaKey || event.shiftKey ? selection : null;
    let x = event.clientX;
    let y = event.clientY;
    let active = false;
    let raf = 0;

    const update = () => {
      const box = el.getBoundingClientRect();
      const sx = box.left + startX - el.scrollLeft;
      const sy = box.top + startY - el.scrollTop;
      const rect = { left: Math.min(sx, x), top: Math.min(sy, y), right: Math.max(sx, x), bottom: Math.max(sy, y) };
      const left = Math.max(rect.left, box.left);
      const top = Math.max(rect.top, box.top);
      setMarquee({
        left,
        top,
        width: Math.max(0, Math.min(rect.right, box.right) - left),
        height: Math.max(0, Math.min(rect.bottom, box.bottom) - top),
      });
      const hits: { id: string; space: string }[] = [];
      for (const card of el.querySelectorAll<HTMLElement>("[data-select]")) {
        const r = card.getBoundingClientRect();
        if (r.left < rect.right && r.right > rect.left && r.top < rect.bottom && r.bottom > rect.top) {
          hits.push({ id: card.dataset.select!, space: card.dataset.selectSpace! });
        }
      }
      const space = startSpace ?? hits[0]?.space ?? base?.space;
      if (!space) return setSelection(null);
      const ids = hits.filter((h) => h.space === space).map((h) => h.id);
      setSelection({ space, ids: base?.space === space ? [...new Set([...base.ids, ...ids])] : ids });
    };

    // Near the top or bottom edge, keep scrolling so the box can grow past what's on screen.
    const autoScroll = () => {
      raf = requestAnimationFrame(autoScroll);
      const speed = active ? edgeSpeed(y, el.getBoundingClientRect()) : 0;
      if (speed) {
        el.scrollTop += speed;
        update();
      }
    };

    const onMove = (e: { clientX: number; clientY: number }) => {
      x = e.clientX;
      y = e.clientY;
      if (!active && Math.hypot(x - event.clientX, y - event.clientY) < MARQUEE_THRESHOLD) return;
      active = true;
      update();
    };
    const onScroll = () => active && update();
    // A click on the background (no box) clears the selection.
    const onUp = () => {
      if (!active && !base) setSelection(null);
      stop();
    };
    const stop = () => {
      cancelAnimationFrame(raf);
      window.removeEventListener("pointermove", onMove);
      window.removeEventListener("pointerup", onUp);
      window.removeEventListener("pointercancel", stop);
      window.removeEventListener("blur", stop);
      el.removeEventListener("scroll", onScroll);
      setMarquee(null);
      stopMarquee.current = null;
    };

    window.addEventListener("pointermove", onMove);
    window.addEventListener("pointerup", onUp);
    window.addEventListener("pointercancel", stop);
    window.addEventListener("blur", stop);
    el.addEventListener("scroll", onScroll);
    raf = requestAnimationFrame(autoScroll);
    stopMarquee.current = stop;
  };

  // ------------------------------------------------------------ actions

  const openIn = (space: Space, canvasId: string) =>
    space.server ? onOpenRemote(space.server, canvasId) : onOpen(canvasId);

  const openCanvas = async (space: Space, canvas: Card) => {
    // Shared canvases while sharing is off: turn it back on, then open.
    if (space.paused) {
      const host = await startSharing();
      if (host) onOpenRemote(host, canvas.id);
      return;
    }
    openIn(space, canvas.id);
  };

  /** Turns sharing on; null (after saying why) if it couldn't start. */
  const startSharing = async () => {
    try {
      const status = await desktop.host.update({ enabled: true });
      if (!status.running) {
        ui.alert("Couldn’t start sharing", status.error ?? "Something went wrong.");
        return null;
      }
      setHostStatus(status);
      const list = await refreshServers();
      await loadRemote(HOST);
      return list.find((s) => s.isHost) ?? null;
    } catch (err) {
      alertError("Couldn’t start sharing", err);
      return null;
    }
  };

  const create = async (space: Space, name: string) => {
    setCreatingIn(null);
    try {
      const folder = space.folder?.id;
      if (space.id === LOCAL) return onOpen((await desktop.create(name.trim() || "Untitled", folder)).id);
      const meta = await desktop.remote.create(space.server!.id, name.trim() || "Untitled", folder);
      onOpenRemote(space.server!, meta.id);
    } catch (err) {
      alertError("Couldn’t create the canvas", err);
    }
  };

  const rename = async (space: Space, id: string, name: string) => {
    setRenaming(null);
    if (name.trim()) {
      try {
        if (space.id === LOCAL) await desktop.rename(id, name);
        else await desktop.remote.rename(space.id, id, name);
      } catch (err) {
        alertError("Couldn’t rename", err);
      }
    }
    reload(space.id);
  };

  /** Deletes canvases and folders of a space; other canvases in those folders move back out of them. */
  const remove = async (space: Space, cards: Card[], folders: Folder[] = []) => {
    if (!cards.length && folders.length === 1) return removeFolder(space, folders[0]);
    if (!cards.length && !folders.length) return;
    const one = cards.length === 1 && !folders.length;
    const doomed = new Set(cards.map((c) => c.id));
    const emptied = new Set(folders.map((f) => f.id));
    const orphans = (space.canvases ?? []).filter((c) => !doomed.has(c.id) && emptied.has(folderOf(space.folders, c) ?? "")).length;
    const ok = await ui.confirm({
      title: one ? `Delete “${cards[0].name}”?` : `Delete ${describe(cards.length, folders.length)}?`,
      message:
        [
          orphans &&
            `The ${plural(orphans, "canvas", "canvases")} in ${folders.length === 1 ? "that folder" : "those folders"} ${
              orphans === 1 ? "moves" : "move"
            } back to ${space.title}.`,
          cards.length &&
            (space.id === LOCAL
              ? "This can’t be undone."
              : `${one ? "It’s" : "They’re"} deleted for everyone. This can’t be undone.`),
        ]
          .filter(Boolean)
          .join(" ") || "They’re empty.",
      confirmLabel: "Delete",
      danger: true,
    });
    if (!ok) return;
    for (const canvas of cards) {
      try {
        if (space.id === LOCAL) await desktop.remove(canvas.id);
        else await desktop.remote.remove(space.id, canvas.id);
      } catch (err) {
        alertError(`Couldn’t delete “${canvas.name}”`, err);
      }
    }
    for (const folder of folders) {
      try {
        if (space.id === LOCAL) await desktop.folders.remove(folder.id);
        else await desktop.remote.folders.remove(space.id, folder.id);
      } catch (err) {
        alertError(`Couldn’t delete “${folder.name}”`, err);
      }
    }
    forget(space.id, [...doomed, ...emptied]);
    setSelection(null);
    reload(space.id);
  };

  /** Moves canvases to another space: into `folder` there, by default the one open in it. */
  const move = async (cards: Card[], from: Space, to: Space, index: number | null, folder = to.folder?.id ?? null) => {
    if (!cards.length) return;
    if (to.error) return ui.alert(`Can’t reach ${to.title} right now`, to.error);
    const one = cards.length === 1;
    const toFriend = to.id !== LOCAL && to.id !== HOST;
    // Someone else's server, or people may be drawing in them: check first.
    if (toFriend || (from.id !== LOCAL && (from.id !== HOST || cards.some((c) => c.peers?.length)))) {
      const ok = await ui.confirm({
        title: `Move ${one ? `“${cards[0].name}”` : plural(cards.length, "canvas", "canvases")} to ${to.title}?`,
        message: [
          toFriend && `Everyone on ${to.title} can open and edit ${one ? "it" : "them"}.`,
          from.id !== LOCAL && `${one ? "It disappears" : "They disappear"} from ${from.title} for everyone.`,
        ]
          .filter(Boolean)
          .join(" "),
        confirmLabel: "Move",
      });
      if (!ok) return;
    }
    setMoving(cards.map((c) => c.id));
    const copies: string[] = [];
    try {
      if (to.id === HOST && to.paused && !(await startSharing())) return;
      for (const canvas of cards) {
        try {
          const copy = await desktop.move(
            canvas.id,
            canvas.name,
            from.id === LOCAL ? null : from.id,
            to.id === LOCAL ? null : to.id,
            folder ?? undefined,
          );
          copies.push(copy.id);
          const color = colorOf(from.id, canvas.id);
          if (color) {
            saveColors((all) => {
              all[`${to.id}/${copy.id}`] = color;
              delete all[`${from.id}/${canvas.id}`];
            });
          }
        } catch (err) {
          alertError(`Couldn’t move “${canvas.name}”`, err);
          break;
        }
      }
      await Promise.all([reload(from.id), reload(to.id)]);
      if (!copies.length) return;
      if (folder === (to.folder?.id ?? null)) {
        place(to, copies, index);
        setLanded(copies[0]);
        setSelection({ space: to.id, ids: copies });
      } else {
        setLanded(folder);
      }
    } catch (err) {
      alertError("Couldn’t move", err);
    } finally {
      setMoving([]);
    }
  };

  /** Files canvases in a folder of their space, or takes them out (null). */
  const file = async (space: Space, cards: Card[], folder: string | null) => {
    const todo = cards.filter((c) => folderOf(space.folders, c) !== folder);
    if (!todo.length) return;
    setMoving(todo.map((c) => c.id));
    try {
      for (const canvas of todo) {
        if (space.id === LOCAL) await desktop.setFolder(canvas.id, folder);
        else await desktop.remote.setFolder(space.id, canvas.id, folder);
      }
      await reload(space.id);
      setLanded(folder ?? todo[0].id);
    } catch (err) {
      alertError(todo.length === 1 ? `Couldn’t move “${todo[0].name}”` : "Couldn’t move the canvases", err);
      reload(space.id);
    } finally {
      setMoving([]);
    }
  };

  const onDrop = (cards: Card[], fromId: string, target: DropTarget) => {
    const from = spaceOf(fromId);
    if (target.kind === "trash") return remove(from, cards);
    if (target.kind === "folder") {
      if (target.space === fromId) return file(from, cards, target.folder);
      return move(cards, from, spaceOf(target.space), null, target.folder);
    }
    if (target.space !== fromId) return move(cards, from, spaceOf(target.space), target.index);
    if (target.index !== null) place(from, cards.map((c) => c.id), target.index);
  };

  // ------------------------------------------------------------ folders

  const createFolder = async (space: Space, name: string, canvases: Card[] = []) => {
    setNewFolder(null);
    if (!name.trim()) return;
    try {
      const folder =
        space.id === LOCAL ? await desktop.folders.create(name) : await desktop.remote.folders.create(space.id, name);
      for (const canvas of canvases) {
        if (space.id === LOCAL) await desktop.setFolder(canvas.id, folder.id);
        else await desktop.remote.setFolder(space.id, canvas.id, folder.id);
      }
      await reload(space.id);
      setLanded(folder.id);
    } catch (err) {
      alertError("Couldn’t create the folder", err);
      reload(space.id);
    }
  };

  /** Starts naming a new folder at the top level of a space. */
  const startNewFolder = (space: Space, canvases?: Card[]) => {
    enter(space.id, null);
    setNewFolder({ space: space.id, canvases });
  };

  const renameFolder = async (space: Space, folder: Folder, name: string) => {
    setRenaming(null);
    if (name.trim()) {
      try {
        if (space.id === LOCAL) await desktop.folders.rename(folder.id, name);
        else await desktop.remote.folders.rename(space.id, folder.id, name);
      } catch (err) {
        alertError("Couldn’t rename", err);
      }
    }
    reload(space.id);
  };

  const removeFolder = async (space: Space, folder: Folder) => {
    const count = (space.canvases ?? []).filter((c) => folderOf(space.folders, c) === folder.id).length;
    const ok = await ui.confirm({
      title: `Delete the folder “${folder.name}”?`,
      message: count
        ? `The ${count === 1 ? "canvas" : `${count} canvases`} in it ${count === 1 ? "isn’t" : "aren’t"} deleted: ${
            count === 1 ? "it moves" : "they move"
          } back to ${space.title}${space.id === LOCAL ? "" : ", for everyone"}.`
        : "It’s empty.",
      confirmLabel: "Delete Folder",
    });
    if (!ok) return;
    try {
      if (space.id === LOCAL) await desktop.folders.remove(folder.id);
      else await desktop.remote.folders.remove(space.id, folder.id);
    } catch (err) {
      alertError(`Couldn’t delete “${folder.name}”`, err);
    }
    if (space.folder?.id === folder.id) enter(space.id, null);
    forget(space.id, [folder.id]);
    reload(space.id);
  };

  const showFolderMenu = async (space: Space, folder: Folder, anchor: Anchor) => {
    const action = await ui.menu(
      [
        { id: "open", text: "Open" },
        "separator",
        { id: "rename", text: "Rename…" },
        { id: "copy", text: "Copy" },
        colorMenu(),
        "separator",
        { id: "delete", text: "Delete Folder…" },
      ],
      anchor,
    );
    if (action?.startsWith("color:")) paint(space.id, [folder.id], action.slice(6) || null);
    if (action === "open") enter(space.id, folder.id);
    if (action === "copy") copy(space, [], [folder]);
    if (action === "rename") setRenaming(folder.id);
    if (action === "delete") removeFolder(space, folder);
  };

  const { drag, start } = useCardDrag<Card[]>(scroller, onDrop);

  const copyInvite = async (serverId: string, canvasId?: string) => {
    try {
      await desktop.copyText(await desktop.servers.inviteLink(serverId, canvasId));
      return true;
    } catch (err) {
      alertError("Couldn’t copy the invite link", err);
      return false;
    }
  };

  /** "Move to" entries for canvases of a space: its folders, out of theirs, a new folder, the other spaces. */
  const destinations = (space: Space, canvases: Card[]): MenuEntry[] => {
    const homes = new Set(canvases.map((c) => folderOf(space.folders, c)));
    const current = homes.size === 1 ? [...homes][0] : null;
    const inFolders: MenuEntry[] = space.folders
      ? [
          ...space.folders.filter((f) => f.id !== current).map((f) => ({ id: `file:${f.id}`, text: f.name })),
          ...(current
            ? [{ id: "file:", text: `Out of “${space.folders.find((f) => f.id === current)?.name}”` }]
            : [...homes].some(Boolean)
              ? [{ id: "file:", text: "Out of Folders" }]
              : []),
          { id: "new-folder", text: "New Folder…" },
        ]
      : [];
    const elsewhere: MenuEntry[] = spaces
      .filter((s) => s.id !== space.id)
      .map((s) => ({ id: `move:${s.id}`, text: s.title, enabled: !s.error }));
    return [...inFolders, ...(inFolders.length && elsewhere.length ? ["separator" as const] : []), ...elsewhere];
  };

  /** Carries out a pick from `destinations`; false if it wasn't one. */
  const moveTo = (action: string, space: Space, canvases: Card[]) => {
    if (action.startsWith("move:")) move(canvases, space, spaceOf(action.slice(5)), 0);
    else if (action.startsWith("file:")) file(space, canvases, action.slice(5) || null);
    else if (action === "new-folder") startNewFolder(space, canvases);
    else return false;
    return true;
  };

  const showSelectionMenu = async (space: Space, anchor: Anchor) => {
    const { canvases, folders } = picked(space);
    const action = await ui.menu(
      [
        { id: "copy", text: "Copy" },
        colorMenu(),
        ...(canvases.length
          ? [{ text: folders.length ? `Move ${plural(canvases.length, "Canvas", "Canvases")} to` : "Move to", items: destinations(space, canvases) }]
          : []),
        "separator",
        { id: "delete", text: `Delete ${describe(canvases.length, folders.length)}…` },
      ],
      anchor,
    );
    if (!action || moveTo(action, space, canvases)) return;
    if (action.startsWith("color:")) paint(space.id, [...folders, ...canvases].map((c) => c.id), action.slice(6) || null);
    if (action === "copy") copy(space, canvases, folders);
    if (action === "delete") remove(space, canvases, folders);
  };

  const showCardMenu = async (space: Space, canvas: Card, anchor: Anchor) => {
    const action = await ui.menu(
      [
        { id: "open", text: "Open" },
        "separator",
        { id: "rename", text: "Rename…" },
        { id: "copy", text: "Copy" },
        { id: "duplicate", text: "Duplicate" },
        colorMenu(),
        { text: "Move to", items: destinations(space, [canvas]) },
        ...(space.server
          ? [
              { id: "invite", text: "Copy Invite Link" },
              { id: "ai", text: "Connect AI Assistant…" },
            ]
          : []),
        { id: "export", text: "Export…" },
        ...(space.id === LOCAL ? [{ id: "reveal", text: "Show in Finder" }] : []),
        "separator",
        { id: "delete", text: "Delete…" },
      ],
      anchor,
    );
    if (!action || moveTo(action, space, [canvas])) return;
    if (action.startsWith("color:")) return paint(space.id, [canvas.id], action.slice(6) || null);
    try {
      switch (action) {
        case "open":
          return openIn(space, canvas.id);
        case "rename":
          return setRenaming(canvas.id);
        case "copy":
          return copy(space, [canvas]);
        case "duplicate":
          if (space.id === LOCAL) await desktop.duplicate(canvas.id);
          else await desktop.remote.duplicate(space.id, canvas.id);
          return reload(space.id);
        case "invite":
          return copyInvite(space.id, canvas.id);
        case "ai":
          return setPanel({ kind: "ai", serverId: space.id, canvasId: canvas.id, canvasName: canvas.name });
        case "export":
          if (space.id === LOCAL) return desktop.exportCanvas(canvas.id);
          return await desktop.remote.exportCanvas(space.id, canvas.id, canvas.name);
        case "reveal":
          return desktop.reveal(canvas.id);
        case "delete":
          return remove(space, [canvas]);
      }
    } catch (err) {
      alertError("Something went wrong", err);
    }
  };

  const showServerMenu = async (space: Space, anchor: Anchor) => {
    const action = await ui.menu([{ id: "remove", text: "Leave" }], anchor);
    if (action !== "remove") return;
    const ok = await ui.confirm({
      title: `Leave ${space.title}?`,
      message: "Its canvases stay there. You can join again from Join.",
      confirmLabel: "Leave",
    });
    if (!ok) return;
    await desktop.servers.remove(space.id);
    refreshServers();
  };

  // ------------------------------------------------------------ copy and paste

  /** Copies cards of a space, to paste with ⌘V. Their names go on the clipboard too, which is how a paste recognises them. */
  const copy = async (space: Space, canvases: Card[], folders: Folder[] = []) => {
    if (space.paused || (!canvases.length && !folders.length)) return;
    const text = [...folders, ...canvases].map((c) => c.name).join("\n");
    clip = {
      space: space.id,
      canvases,
      folders: folders.map((folder) => ({
        folder,
        canvases: (space.canvases ?? []).filter((c) => folderOf(space.folders, c) === folder.id),
      })),
      text,
    };
    try {
      await desktop.copyText(text);
    } catch (err) {
      alertError("Couldn’t copy", err);
    }
  };

  /** Pastes copied cards into a space: canvases into the folder open there, folders (with their canvases) at its top level. */
  const paste = async (copied: Clip, to: Space) => {
    const from = spaces.find((s) => s.id === copied.space);
    if (!from) return ui.alert("Couldn’t paste", "The canvases were copied from a server you’ve since left.");
    if (from.error) return ui.alert(`Can’t reach ${from.title} right now`, from.error);
    if (to.error) return ui.alert(`Can’t reach ${to.title} right now`, to.error);
    if (to.id === HOST && to.paused && !(await startSharing())) return;
    // Next to the originals, copies are told apart by name.
    const named = (name: string) => (from.id === to.id ? `${name} copy` : name);
    const pasted: string[] = [];
    const paste1 = async (canvas: Card, name: string, folder: string | null) => {
      const made = await desktop.copy(
        canvas.id,
        name,
        from.id === LOCAL ? null : from.id,
        to.id === LOCAL ? null : to.id,
        folder ?? undefined,
      );
      const color = colorOf(from.id, canvas.id);
      if (color) paint(to.id, [made.id], color);
      return made.id;
    };
    try {
      for (const { folder, canvases } of copied.folders) {
        if (!to.folders) {
          // A server without folders: just the canvases.
          for (const canvas of canvases) pasted.push(await paste1(canvas, canvas.name, null));
          continue;
        }
        const made =
          to.id === LOCAL
            ? await desktop.folders.create(named(folder.name))
            : await desktop.remote.folders.create(to.id, named(folder.name));
        const color = colorOf(from.id, folder.id);
        if (color) paint(to.id, [made.id], color);
        pasted.push(made.id);
        for (const canvas of canvases) await paste1(canvas, canvas.name, made.id);
      }
      for (const canvas of copied.canvases) pasted.push(await paste1(canvas, named(canvas.name), to.folder?.id ?? null));
    } catch (err) {
      alertError("Couldn’t paste everything", err);
    }
    await reload(to.id);
    if (pasted.length) {
      setSelection({ space: to.id, ids: pasted });
      setLanded(pasted[0]);
    }
  };

  /** Where ⌘V pastes: the space with the selection, else the one last worked in. */
  const pasteTarget = () => spaces.find((s) => s.id === (selection?.space ?? activeSpace.current)) ?? spaceOf(LOCAL);

  // Pasting copied cards anywhere on this screen pastes them; pasting an invite link joins it.
  onPaste.current = (e) => {
    if (panel || ui.isOpen() || (e.target as HTMLElement).closest?.("input, textarea")) return;
    const text = e.clipboardData?.getData("text").trim() ?? "";
    if (clip && text === clip.text.trim()) {
      e.preventDefault();
      paste(clip, pasteTarget());
      return;
    }
    if (!text.startsWith("drawlycool:")) return;
    e.preventDefault();
    setPanel({ kind: "connect", link: text, auto: Boolean(parseInvite(text)) });
  };

  /** Right-click on a space's background: paste there. */
  const onBackgroundMenu = async (event: MouseEvent<HTMLDivElement>) => {
    if ((event.target as HTMLElement).closest("button, input, textarea, a, [role=button], [data-select]")) return;
    const id = (event.target as HTMLElement).closest<HTMLElement>("[data-drop=space]")?.dataset.space;
    const space = spaces.find((s) => s.id === id);
    if (!space) return;
    event.preventDefault();
    activeSpace.current = space.id;
    const action = await ui.menu([{ id: "paste", text: "Paste", enabled: clip !== null && !space.error }], anchorOf(event));
    if (action === "paste" && clip) paste(clip, space);
  };

  // ⌘A selects everything a space shows, Escape nothing, ⌘C copies; Delete and Enter act on the selection.
  onKey.current = (e) => {
    if (panel || drag || ui.isOpen() || (e.target as HTMLElement).closest?.("input, textarea")) return;
    const space = selection && spaces.find((s) => s.id === selection.space);
    if (e.key === "a" && e.metaKey) {
      e.preventDefault();
      const target = space ?? spaceOf(LOCAL);
      setSelection({ space: target.id, ids: itemsOf(target) });
      return;
    }
    if (!space) return;
    const { canvases, folders } = picked(space);
    if (e.key === "Escape") {
      setSelection(null);
    } else if (e.key === "c" && e.metaKey) {
      e.preventDefault();
      copy(space, canvases, folders);
    } else if ((e.key === "Backspace" || e.key === "Delete") && !space.paused) {
      e.preventDefault();
      remove(space, canvases, folders);
    } else if (e.key === "Enter" && canvases.length + folders.length === 1) {
      e.preventDefault();
      if (folders.length) enter(space.id, folders[0].id);
      else openCanvas(space, canvases[0]);
    }
  };

  // ------------------------------------------------------------ files from Finder

  const onFileDrop = async (event: DragEvent) => {
    event.preventDefault();
    setFileDrag(false);
    const files = await Promise.all(
      [...event.dataTransfer.files].map(async (f) => ({ name: f.name, text: await f.text() })),
    );
    if (!files.length) return;
    const { imported, errors } = await desktop.importData(files);
    if (errors.length) ui.alert("Some files couldn’t be imported", errors.join("\n"));
    if (imported.length) refreshLocal();
  };

  // ------------------------------------------------------------ layout

  const inviteReady = hostStatus !== null && (!hostStatus.internet || hostStatus.tunnel.state === "ready");

  const newFolderButton = (space: Space) =>
    space.folders &&
    !space.paused && <IconButton icon="folderPlus" title="New folder" onClick={() => startNewFolder(space)} />;

  const headerFor = (space: Space) => {
    if (space.id === LOCAL) return <span className="space__actions">{newFolderButton(space)}</span>;
    if (space.id === HOST && space.paused) {
      return space.canvases?.length ? (
        <>
          <span className="space__meta" title="Friends can’t open these until sharing is on">
            <DrawnIcon name="moon" size={16} /> off
          </span>
          <SketchButton seed={23} onClick={startSharing}>
            Turn on
          </SketchButton>
        </>
      ) : null;
    }
    return (
      <>
        <span className="space__meta">
          <span
            className={`status-dot ${
              space.error ? "status-dot--off" : space.id === HOST && !inviteReady ? "status-dot--wait" : "status-dot--on"
            }`}
          />
          {space.id === HOST && <HostNote status={hostStatus} onSetup={() => setPanel({ kind: "host" })} />}
        </span>
        <span className="space__actions">
          {!space.error && (
            <>
              {newFolderButton(space)}
              <CopyButton
                disabled={space.id === HOST && !inviteReady}
                title="Copy invite link"
                onCopy={() => copyInvite(space.id)}
              />
              <IconButton icon="sparkles" title="Invite an AI assistant" onClick={() => setPanel({ kind: "ai", serverId: space.id })} />
            </>
          )}
          {space.id === HOST ? (
            <IconButton icon="gear" title="Sharing settings" onClick={() => setPanel({ kind: "host" })} />
          ) : (
            <IconButton icon="more" title="More" onClick={(e) => showServerMenu(space, anchorOf(e))} />
          )}
        </span>
      </>
    );
  };

  const bodyFor = (space: Space) => {
    if (space.error) {
      return (
        <p className="space__error">
          {space.error}
          <IconButton icon="retry" title="Try again" onClick={() => loadRemote(space.id)} />
        </p>
      );
    }
    if (space.id === HOST && space.paused && !space.canvases?.length && !space.folders?.length) {
      return <ShareZone over={drag?.over?.kind === "space" && drag.over.space === HOST} onClick={startSharing} />;
    }
    return (
      <CardGrid
        space={space}
        drag={drag}
        creating={creatingIn === space.id}
        creatingFolder={newFolder?.space === space.id}
        renaming={renaming}
        moving={moving}
        landed={landed}
        onNew={() => setCreatingIn(space.id)}
        onCreate={(name) => create(space, name)}
        onCancelCreate={() => setCreatingIn(null)}
        onCreateFolder={(name) => createFolder(space, name, newFolder?.canvases)}
        onCancelCreateFolder={() => setNewFolder(null)}
        onOpenFolder={(folder) => enter(space.id, folder.id)}
        selected={selectedIn(space)}
        colorOf={(id) => colorOf(space.id, id)}
        onSelect={(id, mods) => select(space, id, mods)}
        onPress={(e, id, canvas) => press(space, e, id, canvas)}
        onFolderMenu={
          space.paused
            ? undefined
            : (folder, anchor) => menuFor(space, folder.id, anchor, () => showFolderMenu(space, folder, anchor))
        }
        onRenameFolder={(folder, name) => renameFolder(space, folder, name)}
        onRename={(id, name) => rename(space, id, name)}
        onCancelRename={() => setRenaming(null)}
        onOpen={(canvas) => openCanvas(space, canvas)}
        onMenu={
          space.paused ? undefined : (canvas, anchor) => menuFor(space, canvas.id, anchor, () => showCardMenu(space, canvas, anchor))
        }
      />
    );
  };

  return (
    <div
      className={`home ${fileDrag ? "home--dragging" : ""} ${drag ? "home--carrying" : ""}`}
      onDragOver={(e) => {
        if (e.dataTransfer.types.includes("Files")) {
          e.preventDefault();
          setFileDrag(true);
        }
      }}
      onDragLeave={(e) => {
        if (e.currentTarget === e.target) setFileDrag(false);
      }}
      onDrop={onFileDrop}
    >
      <RoughBox seed={7} radius={32} className="home__frame">
        <div className="home__scroll" ref={scroller} onPointerDown={onBackgroundDown} onContextMenu={onBackgroundMenu}>
          <div className="home__toolbar">
            <button className="tool-button" onClick={() => setPanel({ kind: "connect" })} title="Join a friend’s canvases">
              <RoughBox seed={22} radius={14} className="tool-button__box">
                <DrawnIcon name="join" size={20} />
                Join
              </RoughBox>
            </button>
            <button
              className="avatar avatar--button"
              style={{ background: clientColor(profile.id) }}
              title={`You appear as “${profile.name}”`}
              onClick={() => setPanel({ kind: "profile" })}
            >
              {initials(profile.name)}
            </button>
          </div>

          <h1 className="home__title">Welcome!</h1>

          {spaces.map((space) => {
            const over = drag?.over?.kind === "space" && drag.over.space === space.id && drag.from !== space.id;
            return (
              <section
                key={space.id}
                className={`space${over ? " space--over" : ""}${space.paused ? " space--paused" : ""}`}
                data-drop="space"
                data-space={space.id}
              >
                <div className="space__header">
                  <DrawnIcon name={space.icon} size={30} className="space__icon" />
                  <h2 className="space__title" title={space.server && !space.server.isHost ? space.server.url : undefined}>
                    {space.folder ? (
                      <Breadcrumb
                        space={space}
                        folder={space.folder}
                        over={drag?.over?.kind === "folder" && drag.over.space === space.id && drag.over.folder === null}
                        onBack={() => enter(space.id, null)}
                      />
                    ) : (
                      space.title
                    )}
                  </h2>
                  {headerFor(space)}
                </div>
                {bodyFor(space)}
                {space.folder && space.shown?.length === 0 && creatingIn !== space.id && (
                  <p className="home__hint">Empty. Drag canvases onto this folder to file them here.</p>
                )}
                {space.id === LOCAL && canvases?.length === 0 && creatingIn !== LOCAL && (
                  <p className="home__hint">
                    Create a canvas, or drop <code>.excalidraw</code> files here.
                  </p>
                )}
              </section>
            );
          })}
        </div>
      </RoughBox>

      {drag && <DragLayer drag={drag} spaces={spaces} />}
      {marquee && <div className="marquee" style={marquee} />}

      {panel?.kind === "connect" && (
        <ConnectPanel
          initialLink={panel.link}
          autoSubmit={panel.auto}
          onClose={() => setPanel(null)}
          onConnected={(server, canvasId) => {
            setPanel(null);
            refreshServers();
            if (canvasId) onOpenRemote(server, canvasId);
          }}
        />
      )}
      {panel?.kind === "host" && (
        <HostPanel
          onClose={() => setPanel(null)}
          onChanged={() => {
            refreshServers();
            desktop.host.status().then(setHostStatus);
          }}
        />
      )}
      {panel?.kind === "ai" && (
        <AiConnectPanel
          serverId={panel.serverId}
          canvasId={panel.canvasId}
          canvasName={panel.canvasName}
          onClose={() => setPanel(null)}
          onHostSettings={() => setPanel({ kind: "host" })}
        />
      )}
      {panel?.kind === "profile" && (
        <ProfilePanel profile={profile} onChange={onProfileChange} onClose={() => setPanel(null)} />
      )}
    </div>
  );
}

function listOf(list: RemoteList | undefined) {
  return { canvases: list?.canvases ?? null, folders: list?.folders ?? null, error: list?.error ?? null };
}

/** "Space › Folder" while looking into a folder. The space's name goes back, and takes dropped canvases out. */
function Breadcrumb({ space, folder, over, onBack }: { space: Space; folder: Folder; over: boolean; onBack: () => void }) {
  return (
    <>
      <button
        className={`space__crumb${over ? " space__crumb--over" : ""}`}
        title={`Back to ${space.title}`}
        data-drop="folder"
        data-space={space.id}
        data-folder=""
        onClick={onBack}
      >
        {space.title}
      </button>
      <span className="space__crumb-sep">›</span>
      {folder.name}
    </>
  );
}

// ------------------------------------------------------------------ header bits

function IconButton({
  icon,
  title,
  onClick,
  disabled,
  className,
}: {
  icon: IconName;
  title: string;
  onClick: (e: MouseEvent<HTMLButtonElement>) => void;
  disabled?: boolean;
  className?: string;
}) {
  return (
    <button className={`icon-button ${className ?? ""}`} title={title} aria-label={title} disabled={disabled} onClick={onClick}>
      <DrawnIcon name={icon} size={20} />
    </button>
  );
}

/** A link icon that turns into a tick (and says so) once copied. */
function CopyButton({ title, disabled, onCopy }: { title: string; disabled?: boolean; onCopy: () => Promise<boolean> }) {
  const [copied, setCopied] = useState(false);
  useEffect(() => {
    if (!copied) return;
    const timer = setTimeout(() => setCopied(false), 1600);
    return () => clearTimeout(timer);
  }, [copied]);
  return (
    <span className="copy-button">
      <IconButton
        icon={copied ? "check" : "link"}
        title={disabled ? "The invite link is on its way…" : title}
        disabled={disabled}
        className={copied ? "icon-button--done" : ""}
        onClick={async () => setCopied(await onCopy())}
      />
      {copied && <span className="copy-button__tag">Copied!</span>}
    </span>
  );
}

function SketchButton({ seed, onClick, children }: { seed: number; onClick: () => void; children: ReactNode }) {
  return (
    <button className="tool-button tool-button--small" onClick={onClick}>
      <RoughBox seed={seed} radius={12} className="tool-button__box">
        {children}
      </RoughBox>
    </button>
  );
}

/** The one thing worth saying about sharing, if anything: the internet link isn't ready. */
function HostNote({ status, onSetup }: { status: HostStatus | null; onSetup: () => void }) {
  if (!status?.internet) return null;
  switch (status.tunnel.state) {
    case "starting":
      return <span className="space__pending">getting a link…</span>;
    case "missing":
      return (
        <button className="link-button" onClick={onSetup}>
          one-time setup
        </button>
      );
    case "failed":
      return (
        <button className="link-button" onClick={onSetup}>
          no link: fix it
        </button>
      );
    default:
      return null;
  }
}

/** "With friends" before anything is shared: a place to drop the first canvas. */
function ShareZone({ over, onClick }: { over: boolean; onClick: () => void }) {
  return (
    <button className={`share-zone${over ? " share-zone--over" : ""}`} onClick={onClick}>
      <RoughBox seed={12} dashed className="share-zone__box">
        <DrawnIcon name="friends" size={40} />
        <span className="share-zone__text">Drag a canvas here to draw with friends</span>
      </RoughBox>
    </button>
  );
}

// ------------------------------------------------------------------ cards

type Mods = { metaKey: boolean; shiftKey: boolean };

type GridProps = {
  space: Space;
  drag: CardDrag<Card[]> | null;
  creating: boolean;
  creatingFolder: boolean;
  renaming: string | null;
  moving: string[];
  landed: string | null;
  selected: string[];
  colorOf: (id: string) => string | null;
  onSelect: (id: string, mods: Mods) => void;
  /** A press on a card, which may start dragging it (canvases only). */
  onPress: (event: PointerEvent<HTMLElement>, id: string, canvas?: Card) => void;
  onNew: () => void;
  onCreate: (name: string) => void;
  onCancelCreate: () => void;
  onCreateFolder: (name: string) => void;
  onCancelCreateFolder: () => void;
  onRename: (id: string, name: string) => void;
  onCancelRename: () => void;
  onOpen: (canvas: Card) => void;
  onMenu?: (canvas: Card, anchor: Anchor) => void;
  onOpenFolder: (folder: Folder) => void;
  onFolderMenu?: (folder: Folder, anchor: Anchor) => void;
  onRenameFolder: (folder: Folder, name: string) => void;
};

function CardGrid({
  space,
  drag,
  creating,
  creatingFolder,
  renaming,
  moving,
  landed,
  onNew,
  onCreate,
  onCancelCreate,
  onCreateFolder,
  onCancelCreateFolder,
  selected,
  colorOf,
  ...card
}: GridProps) {
  const ref = useRef<HTMLDivElement>(null);
  useFlip(ref);

  // While cards are carried, leave a gap where they would land.
  const all = space.shown ?? [];
  const carried = new Set(drag?.from === space.id ? drag.item.map((c) => c.id) : []);
  const items: (Card | "gap")[] = all.filter((c) => !carried.has(c.id));
  const over = drag?.over;
  if (drag && over?.kind === "space" && over.space === space.id && over.index !== null) {
    items.splice(Math.min(over.index, items.length), 0, "gap");
  } else if (carried.size && (!over || ((over.kind === "space" || over.kind === "folder") && over.space === space.id))) {
    items.splice(all.findIndex((c) => carried.has(c.id)), 0, "gap");
  }

  // Folders come first, at the top level only.
  const folders = space.folder ? [] : (space.folders ?? []);
  const counts = new Map<string, number>();
  for (const c of space.canvases ?? []) {
    const f = folderOf(space.folders, c);
    if (f) counts.set(f, (counts.get(f) ?? 0) + 1);
  }
  const lead = (space.paused ? 0 : 1) + (creatingFolder ? 1 : 0) + folders.length;

  const seedBase = seedFrom(space.id);
  return (
    <div className="home__grid" ref={ref} data-grid={space.paused ? undefined : ""} data-lead={lead}>
      {!space.paused &&
        (creating ? (
          <RoughBox seed={seedBase + 1} className="card card--editing" data-flip="new">
            <NameInput placeholder="Name your canvas" onSubmit={onCreate} onCancel={onCancelCreate} />
          </RoughBox>
        ) : (
          <NewCard seed={seedBase + 1} onClick={onNew} />
        ))}
      {creatingFolder && (
        <RoughBox seed={seedBase + 3} className="card card--editing card--folder" data-flip="new-folder">
          <DrawnIcon name="folder" size={30} className="card__folder-icon" />
          <NameInput placeholder="Name your folder" onSubmit={onCreateFolder} onCancel={onCancelCreateFolder} />
        </RoughBox>
      )}
      {folders.map((f) =>
        renaming === f.id ? (
          <RoughBox key={f.id} seed={seedFrom(f.id)} className="card card--editing card--folder" data-flip={f.id}>
            <DrawnIcon name="folder" size={30} className="card__folder-icon" />
            <NameInput initial={f.name} onSubmit={(name) => card.onRenameFolder(f, name)} onCancel={card.onCancelRename} />
          </RoughBox>
        ) : (
          <FolderCard
            key={f.id}
            space={space.id}
            folder={f}
            count={counts.get(f.id) ?? 0}
            over={over?.kind === "folder" && over.space === space.id && over.folder === f.id}
            landed={landed === f.id}
            selected={selected.includes(f.id)}
            color={colorOf(f.id)}
            onSelect={(mods) => card.onSelect(f.id, mods)}
            onPress={(e) => card.onPress(e, f.id)}
            onOpen={() => card.onOpenFolder(f)}
            onMenu={card.onFolderMenu && ((anchor) => card.onFolderMenu!(f, anchor))}
          />
        ),
      )}
      {items.map((c) =>
        c === "gap" ? (
          <RoughBox key="gap" seed={seedBase + 2} dashed className="card card--gap" data-flip="gap" />
        ) : renaming === c.id ? (
          <RoughBox key={c.id} seed={seedFrom(c.id)} className="card card--editing" data-flip={c.id}>
            <NameInput initial={c.name} onSubmit={(name) => card.onRename(c.id, name)} onCancel={card.onCancelRename} />
          </RoughBox>
        ) : (
          <CanvasCard
            key={c.id}
            space={space.id}
            canvas={c}
            paused={space.paused}
            moving={moving.includes(c.id)}
            landed={landed === c.id}
            selected={selected.includes(c.id)}
            color={colorOf(c.id)}
            onSelect={(mods) => card.onSelect(c.id, mods)}
            onPress={(e) => card.onPress(e, c.id, c)}
            onOpen={() => card.onOpen(c)}
            onMenu={card.onMenu && ((anchor) => card.onMenu!(c, anchor))}
          />
        ),
      )}
    </div>
  );
}

/** Cards glide to their new spot when others move around them. */
function useFlip(container: RefObject<HTMLElement | null>) {
  const before = useRef(new Map<string, { x: number; y: number }>());
  useLayoutEffect(() => {
    const el = container.current;
    if (!el) return;
    const after = new Map<string, { x: number; y: number }>();
    for (const child of el.children as HTMLCollectionOf<HTMLElement>) {
      const key = child.dataset.flip;
      if (!key) continue;
      // Layout position: unaffected by scrolling and by the animations below.
      const pos = { x: child.offsetLeft, y: child.offsetTop };
      after.set(key, pos);
      const prev = before.current.get(key);
      if (prev && (prev.x !== pos.x || prev.y !== pos.y)) {
        child.getAnimations().forEach((a) => a.cancel());
        child.animate(
          [{ transform: `translate(${prev.x - pos.x}px, ${prev.y - pos.y}px)` }, { transform: "none" }],
          { duration: 180, easing: "cubic-bezier(.2,.7,.3,1)" },
        );
      }
    }
    before.current = after;
  });
}

function NewCard({ seed, onClick }: { seed: number; onClick: () => void }) {
  return (
    <button className="card-button" onClick={onClick} data-flip="new" title="New canvas">
      <RoughBox seed={seed} dashed className="card card--new">
        <DrawnIcon name="plus" size={34} className="card__plus" />
        <span className="card__name">New canvas</span>
      </RoughBox>
    </button>
  );
}

/** Classes and colour shared by every kind of card button. */
function cardLook(selected: boolean, color: string | null, ...more: (string | false)[]) {
  return {
    className: ["card-button", selected && "card-button--selected", color && "card-button--colored", ...more]
      .filter(Boolean)
      .join(" "),
    style: color ? ({ "--card-color": colorVar(color) } as CSSProperties) : undefined,
  };
}

/** A folder: opens on click (⌘/shift-click selects), and takes canvases dropped onto it. */
function FolderCard({
  space,
  folder,
  count,
  over,
  landed,
  selected,
  color,
  onSelect,
  onPress,
  onOpen,
  onMenu,
}: {
  space: string;
  folder: Folder;
  count: number;
  over: boolean;
  landed: boolean;
  selected: boolean;
  color: string | null;
  onSelect: (mods: Mods) => void;
  onPress: (event: PointerEvent<HTMLElement>) => void;
  onOpen: () => void;
  onMenu?: (anchor: Anchor) => void;
}) {
  const ref = useRef<HTMLButtonElement>(null);
  useEffect(() => {
    if (landed) ref.current?.scrollIntoView({ block: "nearest", behavior: "smooth" });
  }, [landed]);

  return (
    <button
      ref={ref}
      {...cardLook(selected, color, "card-button--folder", over && "card-button--over", landed && "card-button--landed")}
      data-flip={folder.id}
      data-drop="folder"
      data-space={space}
      data-folder={folder.id}
      data-select={folder.id}
      data-select-space={space}
      title={folder.name}
      onClick={(e) => (e.metaKey || e.shiftKey ? onSelect(e) : onOpen())}
      onPointerDown={(e) => {
        if (!(e.target as HTMLElement).closest(".card__more")) onPress(e);
      }}
      onContextMenu={(e) => {
        e.preventDefault();
        onMenu?.(anchorOf(e));
      }}
    >
      <RoughBox seed={seedFrom(folder.id)} className="card card--folder">
        <DrawnIcon name="folder" size={30} className="card__folder-icon" />
        <span className="card__name">{folder.name}</span>
        <span className="card__meta">{count === 1 ? "1 canvas" : `${count} canvases`}</span>
        {onMenu && (
          <span
            role="button"
            tabIndex={-1}
            className="card__more"
            aria-label="More actions"
            onClick={(e) => {
              e.stopPropagation();
              onMenu(anchorOf(e));
            }}
          >
            <DrawnIcon name="more" size={18} />
          </span>
        )}
      </RoughBox>
    </button>
  );
}

/** A canvas: opens on click (⌘/shift-click selects), and can be dragged around. */
function CanvasCard({
  space,
  canvas,
  paused,
  moving,
  landed,
  selected,
  color,
  onSelect,
  onPress,
  onOpen,
  onMenu,
}: {
  space: string;
  canvas: Card;
  paused: boolean;
  moving: boolean;
  landed: boolean;
  selected: boolean;
  color: string | null;
  onSelect: (mods: Mods) => void;
  onPress: (event: PointerEvent<HTMLElement>) => void;
  onOpen: () => void;
  onMenu?: (anchor: Anchor) => void;
}) {
  const ref = useRef<HTMLButtonElement>(null);
  useEffect(() => {
    if (landed) ref.current?.scrollIntoView({ block: "nearest", behavior: "smooth" });
  }, [landed]);

  return (
    <button
      ref={ref}
      {...cardLook(selected, color, landed && "card-button--landed", paused && "card-button--paused", moving && "card-button--moving")}
      data-flip={canvas.id}
      data-select={canvas.id}
      data-select-space={space}
      title={paused ? `${canvas.name}: opens and turns sharing back on` : canvas.name}
      disabled={moving}
      onClick={(e) => (e.metaKey || e.shiftKey ? onSelect(e) : onOpen())}
      onPointerDown={(e) => {
        if (!(e.target as HTMLElement).closest(".card__more")) onPress(e);
      }}
      onContextMenu={(e) => {
        e.preventDefault();
        onMenu?.(anchorOf(e));
      }}
    >
      <RoughBox seed={seedFrom(canvas.id)} className="card">
        <span className="card__name">{canvas.name}</span>
        <span className="card__meta">Edited {timeAgo(canvas.updatedAt)}</span>
        {canvas.peers && canvas.peers.length > 0 && <PeerStack peers={canvas.peers} />}
        {onMenu && (
          <span
            role="button"
            tabIndex={-1}
            className="card__more"
            aria-label="More actions"
            onClick={(e) => {
              e.stopPropagation();
              onMenu(anchorOf(e));
            }}
          >
            <DrawnIcon name="more" size={18} />
          </span>
        )}
      </RoughBox>
    </button>
  );
}

function PeerStack({ peers }: { peers: Peer[] }) {
  const shown = peers.slice(0, 4);
  return (
    <span className="peer-stack" title={`Here now: ${peers.map((p) => p.name).join(", ")}`}>
      {shown.map((p) => (
        <span key={p.id} className="avatar avatar--small" style={{ background: clientColor(p.userId || p.id) }}>
          {initials(p.name)}
        </span>
      ))}
      {peers.length > shown.length && <span className="peer-stack__more">+{peers.length - shown.length}</span>}
    </span>
  );
}

// ------------------------------------------------------------------ while dragging

/** The card under the pointer, and a dock of every other place it can go (plus the trash). */
function DragLayer({ drag, spaces }: { drag: CardDrag<Card[]>; spaces: Space[] }) {
  const over = drag.over;
  const overTrash = over?.kind === "trash";
  const elsewhere = over?.kind === "space" && over.space !== drag.from;
  const docked = overTrash || (over?.kind === "space" && over.dock);
  return (
    <>
      <div
        className={`drag-ghost${elsewhere ? " drag-ghost--away" : ""}${docked ? " drag-ghost--docked" : ""}${overTrash ? " drag-ghost--trash" : ""}`}
        style={{
          left: drag.x - drag.grabX,
          top: drag.y - drag.grabY,
          width: drag.width,
          height: drag.height,
          // Shrinks toward the pointer, so the target stays visible.
          transformOrigin: `${drag.grabX}px ${drag.grabY}px`,
        }}
      >
        {drag.item.length > 1 && <RoughBox seed={seedFrom(drag.item[1].id)} className="card drag-ghost__under" />}
        <RoughBox seed={seedFrom(drag.item[0].id)} className="card">
          <span className="card__name">{drag.item[0].name}</span>
        </RoughBox>
        {drag.item.length > 1 && <span className="drag-ghost__count">{drag.item.length}</span>}
      </div>
      <div className="dock">
        {spaces
          .filter((s) => s.id !== drag.from && !s.error)
          .map((s) => (
            <DockTarget
              key={s.id}
              icon={s.icon}
              label={s.title}
              hot={over?.kind === "space" && over.space === s.id}
              drop={{ "data-drop": "space", "data-space": s.id, "data-dock": "" }}
            />
          ))}
        <DockTarget icon="trash" label="Delete" hot={overTrash} danger drop={{ "data-drop": "trash" }} />
      </div>
    </>
  );
}

function DockTarget({
  icon,
  label,
  hot,
  danger,
  drop,
}: {
  icon: IconName;
  label: string;
  hot: boolean;
  danger?: boolean;
  drop: Record<string, string>;
}) {
  return (
    <div className={`dock__target${hot ? " dock__target--hot" : ""}${danger ? " dock__target--danger" : ""}`} {...drop}>
      <RoughBox seed={seedFrom(label)} radius={20} className="dock__box">
        <DrawnIcon name={icon} size={28} />
        <span className="dock__label">{label}</span>
      </RoughBox>
    </div>
  );
}

// ------------------------------------------------------------------ name input

export function NameInput({
  initial = "",
  placeholder,
  onSubmit,
  onCancel,
}: {
  initial?: string;
  placeholder?: string;
  onSubmit: (name: string) => void;
  onCancel: () => void;
}) {
  const ref = useRef<HTMLInputElement>(null);
  const done = useRef(false);

  useEffect(() => {
    ref.current?.focus();
    ref.current?.select();
  }, []);

  const finish = (submit: boolean) => {
    if (done.current) return;
    done.current = true;
    if (submit) onSubmit(ref.current?.value ?? "");
    else onCancel();
  };

  return (
    <input
      ref={ref}
      className="card__input"
      defaultValue={initial}
      placeholder={placeholder}
      maxLength={120}
      spellCheck={false}
      onKeyDown={(e) => {
        if (e.key === "Enter") finish(true);
        if (e.key === "Escape") finish(false);
      }}
      // Clicking away commits whatever was typed; an empty new name cancels.
      onBlur={() => finish(Boolean(initial || ref.current?.value.trim()))}
    />
  );
}
