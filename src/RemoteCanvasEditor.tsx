import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  CaptureUpdateAction,
  Excalidraw,
  MainMenu,
  getVisibleSceneBounds,
  isInvisiblySmallElement,
  reconcileElements,
  restoreElements,
  zoomToFitBounds,
} from "@excalidraw/excalidraw";
import type {
  AppState,
  BinaryFileData,
  Collaborator,
  ExcalidrawImperativeAPI,
  ExcalidrawInitialDataState,
  SocketId,
} from "@excalidraw/excalidraw/types";
import {
  desktop,
  type CanvasMeta,
  type Peer,
  type Profile,
  type ServerMessage,
  type Session,
  type SessionEvent,
  type ThemePreference,
  type Viewport,
} from "./desktop";
import {
  CanvasChip,
  EditorMainMenu,
  UI_OPTIONS,
  exposeApi,
  linkIcon,
  sparklesIcon,
  useEditorHousekeeping,
} from "./editorChrome";
import { AiConnectPanel } from "./Panels";

const BROADCAST_DELAY = 50;
const POINTER_DELAY = 40;
const VIEWPORT_SAVE_DELAY = 800;

type RemoteElements = Parameters<typeof reconcileElements>[1];
type Status = "connecting" | "online" | "offline" | "closed";

type Props = {
  serverId: string;
  serverName: string;
  canvasId: string;
  profile: Profile;
  themePreference: ThemePreference;
  effectiveTheme: "light" | "dark";
  onThemeChange: (theme: ThemePreference) => void;
  onGoHome: () => void;
  /** Opens the Host panel (from "Connect an AI assistant"). */
  onHostSettings: () => void;
  /** Lets the app send pending changes before navigating or quitting. */
  registerFlush: (flush: (() => Promise<void>) | null) => void;
};

// Viewports of shared canvases are personal, so they stay on this Mac.
const viewportKey = (serverId: string, canvasId: string) => `viewport:${serverId}:${canvasId}`;

function readViewport(key: string): Viewport | null {
  try {
    const raw = localStorage.getItem(key);
    return raw ? (JSON.parse(raw) as Viewport) : null;
  } catch {
    return null;
  }
}

function writeViewport(key: string, viewport: Viewport) {
  try {
    localStorage.setItem(key, JSON.stringify(viewport));
  } catch {
    // Not important enough to report.
  }
}

/** Sync bookkeeping for one live session. */
type SyncState = {
  me: string;
  /** Highest version of each element we've sent or received. */
  versions: Map<string, number>;
  /** Image files the server already has. */
  files: Set<string>;
  background: string | null;
  peers: Map<string, Peer>;
  pointers: Map<string, Partial<Collaborator>>;
  online: boolean;
  /** "loading": Excalidraw is still taking in the first scene. */
  phase: "waiting" | "loading" | "ready";
  queued: ServerMessage[];
  broadcastTimer?: number;
  pointerTimer?: number;
  pendingPointer: { pointer: unknown; button: string } | null;
  followed: boolean;
  lastBounds: string;
};

const newSyncState = (): SyncState => ({
  me: "",
  versions: new Map(),
  files: new Set(),
  background: null,
  peers: new Map(),
  pointers: new Map(),
  online: false,
  phase: "waiting",
  queued: [],
  pendingPointer: null,
  followed: false,
  lastBounds: "",
});

export function RemoteCanvasEditor({
  serverId,
  serverName,
  canvasId,
  profile,
  themePreference,
  effectiveTheme,
  onThemeChange,
  onGoHome,
  onHostSettings,
  registerFlush,
}: Props) {
  const [meta, setMeta] = useState<CanvasMeta | null>(null);
  const [initialData, setInitialData] = useState<ExcalidrawInitialDataState | null>(null);
  const [status, setStatus] = useState<Status>("connecting");
  const [statusText, setStatusText] = useState<string | null>(null);
  const [fatal, setFatal] = useState<string | null>(null);
  const [renaming, setRenaming] = useState(false);
  const [copied, setCopied] = useState(false);
  const [aiPanel, setAiPanel] = useState(false);

  const apiRef = useRef<ExcalidrawImperativeAPI | null>(null);
  const sessionRef = useRef<Session | null>(null);
  const sync = useRef<SyncState>(newSyncState());
  const profileRef = useRef(profile);
  profileRef.current = profile;
  const vpKey = viewportKey(serverId, canvasId);
  const viewportTimer = useRef<number>(undefined);
  const savedViewport = useRef<string | null>(null);
  const housekeeping = useEditorHousekeeping(apiRef, effectiveTheme, onThemeChange);

  // ------------------------------------------------------------ outgoing

  const send = useCallback((msg: Parameters<Session["send"]>[0]) => {
    if (!sync.current.online) return Promise.resolve();
    return sessionRef.current?.send(msg).catch((err) => console.error("send failed", err)) ?? Promise.resolve();
  }, []);

  /** Sends whatever changed since the last broadcast. */
  const broadcastNow = useCallback(
    async (everything = false) => {
      const s = sync.current;
      window.clearTimeout(s.broadcastTimer);
      s.broadcastTimer = undefined;
      const api = apiRef.current;
      if (!api || !s.online || s.phase !== "ready") return;
      const sends: Promise<void>[] = [];

      const changed = api
        .getSceneElementsIncludingDeleted()
        .filter((el) => everything || el.version > (s.versions.get(el.id) ?? -1))
        .filter((el) => el.isDeleted || !isInvisiblySmallElement(el));
      if (changed.length) {
        for (const el of changed) s.versions.set(el.id, el.version);
        sends.push(send({ type: "update", elements: changed }));
      }

      const newFiles = Object.values(api.getFiles()).filter((f) => !s.files.has(f.id) && f.dataURL);
      if (newFiles.length) {
        for (const f of newFiles) s.files.add(f.id);
        sends.push(send({ type: "files", files: newFiles }));
      }

      const background = api.getAppState().viewBackgroundColor;
      if (background !== s.background) {
        s.background = background;
        sends.push(send({ type: "background", color: background }));
      }
      await Promise.all(sends);
    },
    [send],
  );

  const scheduleBroadcast = useCallback(() => {
    const s = sync.current;
    if (s.broadcastTimer === undefined) s.broadcastTimer = window.setTimeout(() => broadcastNow(), BROADCAST_DELAY);
  }, [broadcastNow]);

  // ------------------------------------------------------------ incoming

  const refreshCollaborators = useCallback(() => {
    const s = sync.current;
    const api = apiRef.current;
    if (!api || s.phase !== "ready") return;
    const me = profileRef.current;
    const collaborators = new Map<SocketId, Collaborator>();
    if (s.me) {
      collaborators.set(s.me as SocketId, {
        id: me.id,
        socketId: s.me as SocketId,
        username: me.name,
        isCurrentUser: true,
      });
    }
    if (s.online) {
      for (const peer of s.peers.values()) {
        collaborators.set(peer.id as SocketId, {
          ...s.pointers.get(peer.id),
          id: peer.userId || peer.id,
          socketId: peer.id as SocketId,
          username: peer.name,
        });
      }
    }
    api.updateScene({ collaborators });
  }, []);

  const applyElements = useCallback((remote: unknown[]) => {
    const api = apiRef.current!;
    const s = sync.current;
    for (const el of remote as { id: string; version: number }[]) {
      s.versions.set(el.id, Math.max(s.versions.get(el.id) ?? -1, el.version));
    }
    const restored = restoreElements(remote as Parameters<typeof restoreElements>[0], null);
    const elements = reconcileElements(
      api.getSceneElementsIncludingDeleted(),
      restored as unknown as RemoteElements,
      api.getAppState(),
    );
    api.updateScene({ elements, captureUpdate: CaptureUpdateAction.NEVER });
  }, []);

  const addFiles = useCallback((files: unknown[]) => {
    const s = sync.current;
    for (const f of files as BinaryFileData[]) s.files.add(f.id);
    if (files.length) apiRef.current!.addFiles(files as BinaryFileData[]);
  }, []);

  /** Applies a server message once the editor is up. */
  const apply = useCallback(
    (msg: ServerMessage) => {
      const api = apiRef.current!;
      const s = sync.current;
      switch (msg.type) {
        case "init": {
          // Reconnected: take in what changed meanwhile, then resend our side
          // in case edits were lost with the connection.
          s.me = msg.you;
          s.peers = new Map(msg.peers.map((p) => [p.id, p]));
          s.pointers.clear();
          setMeta(msg.meta);
          applyElements(msg.elements);
          s.files = new Set(Object.keys(msg.files));
          addFiles(Object.values(msg.files));
          if (msg.background) {
            s.background = msg.background;
            api.updateScene({ appState: { viewBackgroundColor: msg.background }, captureUpdate: CaptureUpdateAction.NEVER });
          }
          refreshCollaborators();
          void broadcastNow(true);
          return;
        }
        case "update":
          return applyElements(msg.elements);
        case "files":
          return addFiles(msg.files);
        case "background":
          s.background = msg.color;
          return api.updateScene({ appState: { viewBackgroundColor: msg.color }, captureUpdate: CaptureUpdateAction.NEVER });
        case "pointer":
          s.pointers.set(msg.from, {
            pointer: msg.pointer,
            button: msg.button,
            selectedElementIds: msg.selectedElementIds ?? {},
          });
          return refreshCollaborators();
        case "peer-joined":
          s.peers.set(msg.peer.id, msg.peer);
          return refreshCollaborators();
        case "peer-left":
          s.peers.delete(msg.id);
          s.pointers.delete(msg.id);
          return refreshCollaborators();
        case "meta":
          return setMeta(msg.meta);
        case "followed-by":
          s.followed = msg.ids.length > 0;
          s.lastBounds = "";
          api.updateScene({ appState: { followedBy: new Set(msg.ids as SocketId[]) } });
          return;
        case "bounds": {
          const appState = api.getAppState();
          if (appState.userToFollow?.socketId !== msg.from) return;
          const next = zoomToFitBounds({ appState, bounds: msg.bounds, fitToViewport: true, viewportZoomFactor: 1 });
          api.updateScene({ appState: next.appState, captureUpdate: CaptureUpdateAction.NEVER });
          return;
        }
        case "deleted":
          setFatal("This canvas was deleted.");
          return;
      }
    },
    [applyElements, addFiles, refreshCollaborators, broadcastNow],
  );

  // ------------------------------------------------------------ session

  useEffect(() => {
    let cancelled = false;
    sync.current = newSyncState();

    const onEvent = (event: SessionEvent) => {
      if (cancelled) return;
      const s = sync.current;
      if ("status" in event) {
        const { state, error, fatal } = event.status;
        if (fatal) {
          // Keep a friendlier message that arrived first (e.g. "deleted").
          setFatal((prev) => prev ?? error ?? "The connection was closed.");
          return;
        }
        s.online = state === "online";
        setStatus(state);
        setStatusText(error);
        if (!s.online) refreshCollaborators();
        return;
      }
      const msg = event.msg;
      if (s.phase === "waiting" && msg.type === "init") {
        // First scene: hand it to Excalidraw as initial data.
        s.me = msg.you;
        s.peers = new Map(msg.peers.map((p) => [p.id, p]));
        s.background = msg.background;
        s.files = new Set(Object.keys(msg.files));
        for (const el of msg.elements as { id: string; version: number }[]) s.versions.set(el.id, el.version);
        const viewport = readViewport(vpKey);
        setMeta(msg.meta);
        setInitialData({
          elements: msg.elements as ExcalidrawInitialDataState["elements"],
          files: msg.files as ExcalidrawInitialDataState["files"],
          appState: {
            viewBackgroundColor: msg.background ?? "#ffffff",
            ...(viewport && { scrollX: viewport.scrollX, scrollY: viewport.scrollY, zoom: { value: viewport.zoom as never } }),
          },
          scrollToContent: !viewport,
        });
        s.phase = "loading";
        return;
      }
      if (s.phase !== "ready") {
        s.queued.push(msg);
        return;
      }
      apply(msg);
    };

    desktop.remote
      .connect(serverId, canvasId, profileRef.current, onEvent)
      .then((session) => {
        if (cancelled) session.close();
        else sessionRef.current = session;
      })
      .catch((err) => !cancelled && setFatal(String(err?.message ?? err)));

    return () => {
      cancelled = true;
      const session = sessionRef.current;
      sessionRef.current = null;
      if (session) {
        // Send what's still pending, then hang up.
        broadcastNow().finally(() => session.close());
      }
      window.clearTimeout(sync.current.pointerTimer);
    };
  }, [serverId, canvasId, vpKey, apply, broadcastNow, refreshCollaborators]);

  useEffect(() => {
    const flush = async () => {
      window.clearTimeout(viewportTimer.current);
      await broadcastNow();
    };
    registerFlush(flush);
    return () => registerFlush(null);
  }, [broadcastNow, registerFlush]);

  useEffect(() => {
    document.title = meta ? `${meta.name} — ${serverName}` : "Drawlycool";
    return () => {
      document.title = "Drawlycool";
    };
  }, [meta, serverName]);

  // ------------------------------------------------------------ editor events

  const onChange = useCallback(
    (_elements: unknown, appState: AppState) => {
      const s = sync.current;
      housekeeping(appState);

      if (s.phase === "loading") {
        // Excalidraw has taken in the first scene: catch up on what arrived meanwhile.
        s.phase = "ready";
        const queued = s.queued.splice(0);
        refreshCollaborators();
        queued.forEach(apply);
      }
      if (s.phase !== "ready") return;
      scheduleBroadcast();

      // Viewport (personal, kept on this Mac).
      const vp = `${appState.scrollX}|${appState.scrollY}|${appState.zoom.value}`;
      if (savedViewport.current === null) savedViewport.current = vp;
      if (vp !== savedViewport.current) {
        window.clearTimeout(viewportTimer.current);
        viewportTimer.current = window.setTimeout(() => {
          savedViewport.current = vp;
          writeViewport(vpKey, { scrollX: appState.scrollX, scrollY: appState.scrollY, zoom: appState.zoom.value });
        }, VIEWPORT_SAVE_DELAY);
      }

      // Someone follows us: share what we're looking at.
      if (s.followed) {
        const bounds = getVisibleSceneBounds(appState);
        const key = bounds.map((n) => Math.round(n)).join(",");
        if (key !== s.lastBounds) {
          s.lastBounds = key;
          send({ type: "bounds", bounds });
        }
      }
    },
    [housekeeping, refreshCollaborators, apply, scheduleBroadcast, vpKey, send],
  );

  const onPointerUpdate = useCallback(
    ({ pointer, button }: { pointer: { x: number; y: number; tool: "pointer" | "laser" }; button: "down" | "up" }) => {
      const s = sync.current;
      s.pendingPointer = { pointer, button };
      if (s.pointerTimer !== undefined) return;
      s.pointerTimer = window.setTimeout(() => {
        s.pointerTimer = undefined;
        const next = s.pendingPointer;
        s.pendingPointer = null;
        if (!next) return;
        const selectedElementIds = apiRef.current?.getAppState().selectedElementIds ?? {};
        send({ type: "pointer", pointer: next.pointer, button: next.button, selectedElementIds });
      }, POINTER_DELAY);
    },
    [send],
  );

  // (The `onUserFollow` prop isn't wired up in this Excalidraw version; the
  // API subscription is.)
  const unsubscribeFollow = useRef<(() => void) | null>(null);
  useEffect(() => () => unsubscribeFollow.current?.(), []);
  const onApi = useCallback(
    (api: ExcalidrawImperativeAPI) => {
      apiRef.current = api;
      exposeApi(api);
      unsubscribeFollow.current?.();
      unsubscribeFollow.current = api.onUserFollow(({ userToFollow, action }) =>
        send({ type: "follow", target: userToFollow.socketId, follow: action === "FOLLOW" }),
      );
    },
    [send],
  );

  // ------------------------------------------------------------ chrome

  const copyInvite = useCallback(async () => {
    try {
      await desktop.copyText(await desktop.servers.inviteLink(serverId, canvasId));
    } catch (err) {
      return desktop.showError("Couldn’t copy the invite link", String(err));
    }
    setCopied(true);
    window.setTimeout(() => setCopied(false), 1600);
  }, [serverId, canvasId]);

  const commitRename = useCallback(
    async (name: string) => {
      try {
        setMeta(await desktop.remote.rename(serverId, canvasId, name));
      } catch (err) {
        console.error("Rename failed", err);
      }
    },
    [serverId, canvasId],
  );

  const renderTopRightUI = useCallback(
    () => (
      <CanvasChip
        name={meta?.name}
        renaming={renaming}
        setRenaming={setRenaming}
        onRename={commitRename}
        onGoHome={onGoHome}
      >
        <button
          className="canvas-chip__icon"
          title={copied ? "Copied!" : "Copy invite link"}
          aria-label="Copy invite link"
          onClick={copyInvite}
        >
          {copied ? <span className="canvas-chip__copied">Copied</span> : linkIcon}
        </button>
        <button
          className="canvas-chip__icon"
          title="Invite an AI assistant"
          aria-label="Invite an AI assistant"
          onClick={() => setAiPanel(true)}
        >
          {sparklesIcon}
        </button>
      </CanvasChip>
    ),
    [meta, renaming, commitRename, onGoHome, copied, copyInvite],
  );

  const mainMenu = useMemo(
    () => (
      <EditorMainMenu
        onGoHome={onGoHome}
        onRename={() => setRenaming(true)}
        themePreference={themePreference}
        onThemeChange={onThemeChange}
        extraItems={
          <>
            <MainMenu.Item icon={linkIcon} onSelect={copyInvite}>
              Copy invite link
            </MainMenu.Item>
            <MainMenu.Item icon={sparklesIcon} onSelect={() => setAiPanel(true)}>
              Invite an AI assistant…
            </MainMenu.Item>
          </>
        }
      />
    ),
    [onGoHome, themePreference, onThemeChange, copyInvite],
  );

  if (fatal) {
    return (
      <div className="canvas-error">
        <p>{fatal}</p>
        <button onClick={onGoHome}>Back to canvases</button>
      </div>
    );
  }

  if (!initialData) {
    return (
      <div className="canvas-loading canvas-loading--remote">
        <p>{status === "offline" ? statusText ?? `Can’t reach ${serverName}. Retrying…` : `Connecting to ${serverName}…`}</p>
        <button onClick={onGoHome}>Back to canvases</button>
      </div>
    );
  }

  const offline = status !== "online";

  return (
    <div className="canvas-editor">
      {offline && (
        <div className="sync-banner" role="status">
          {status === "offline" ? statusText ?? `Can’t reach ${serverName}. Retrying…` : `Reconnecting to ${serverName}…`}
          <span className="sync-banner__hint">View only until the connection is back.</span>
        </div>
      )}
      <Excalidraw
        excalidrawAPI={onApi}
        initialData={initialData}
        onChange={onChange}
        onPointerUpdate={onPointerUpdate}
        isCollaborating
        viewModeEnabled={offline}
        theme={effectiveTheme}
        name={meta?.name}
        aiEnabled={false}
        UIOptions={UI_OPTIONS}
        renderTopRightUI={renderTopRightUI}
        handleKeyboardGlobally
        autoFocus
      >
        {mainMenu}
      </Excalidraw>
      {aiPanel && (
        // Keys typed in the dialog aren't editor shortcuts (Escape still closes it).
        <div onKeyDown={(e) => e.key !== "Escape" && e.stopPropagation()}>
          <AiConnectPanel
            serverId={serverId}
            canvasId={canvasId}
            canvasName={meta?.name}
            onClose={() => setAiPanel(false)}
            onHostSettings={onHostSettings}
          />
        </div>
      )}
    </div>
  );
}
