import { useCallback, useEffect, useRef, useState, type DragEvent } from "react";
import { desktop, type CanvasMeta, type MenuEntry, type Peer, type Profile, type RemoteCanvas, type ServerInfo } from "./desktop";
import { RoughBox, seedFrom } from "./RoughBox";
import { ConnectPanel, HostPanel, ProfilePanel, type Panel } from "./Panels";
import { clientColor, initials } from "./colors";

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
  const [servers, setServers] = useState<ServerInfo[]>([]);
  const [creating, setCreating] = useState(false);
  const [renamingId, setRenamingId] = useState<string | null>(null);
  const [dragging, setDragging] = useState(false);
  const [remoteTick, setRemoteTick] = useState(0);
  const [, forceTick] = useState(0);

  const refresh = useCallback(async () => setCanvases(await desktop.list()), []);
  const refreshServers = useCallback(async () => setServers(await desktop.servers.list()), []);

  useEffect(() => {
    refresh();
    refreshServers();
  }, [refresh, refreshServers, refreshKey]);

  useEffect(() => {
    // Keep "edited x minutes ago" fresh.
    const timer = setInterval(() => forceTick((n) => n + 1), 30_000);
    return () => clearInterval(timer);
  }, []);

  useEffect(() => {
    if (!startNewCanvas) return;
    setCreating(true);
    onStartNewCanvasHandled();
  }, [startNewCanvas, onStartNewCanvasHandled]);

  const create = async (name: string) => {
    setCreating(false);
    const meta = await desktop.create(name.trim() || "Untitled");
    onOpen(meta.id);
  };

  const rename = async (id: string, name: string) => {
    setRenamingId(null);
    if (name.trim()) await desktop.rename(id, name);
    refresh();
  };

  const showMenu = async (canvas: CanvasMeta) => {
    const moveTargets: MenuEntry[] = servers.map((s) => ({ id: `move:${s.id}`, text: s.name }));
    const action = await desktop.contextMenu([
      { id: "open", text: "Open" },
      "separator",
      { id: "rename", text: "Rename…" },
      { id: "duplicate", text: "Duplicate" },
      { text: "Move to", enabled: moveTargets.length > 0, items: moveTargets.length ? moveTargets : [{ id: "none", text: "No servers", enabled: false }] },
      { id: "export", text: "Export…" },
      { id: "reveal", text: "Show in Finder" },
      "separator",
      { id: "delete", text: "Delete…" },
    ]);
    if (action?.startsWith("move:")) {
      const server = servers.find((s) => `move:${s.id}` === action);
      if (!server) return;
      try {
        const moved = await desktop.remote.moveToServer(canvas.id, server.id);
        if (moved) {
          refresh();
          setRemoteTick((n) => n + 1);
        }
      } catch (err) {
        alertError(`Couldn’t move “${canvas.name}”`, err);
      }
      return;
    }
    switch (action) {
      case "open":
        return onOpen(canvas.id);
      case "rename":
        return setRenamingId(canvas.id);
      case "duplicate":
        await desktop.duplicate(canvas.id);
        return refresh();
      case "export":
        return desktop.exportCanvas(canvas.id);
      case "reveal":
        return desktop.reveal(canvas.id);
      case "delete":
        if (await desktop.remove(canvas.id)) refresh();
        return;
    }
  };

  const onDrop = async (event: DragEvent) => {
    event.preventDefault();
    setDragging(false);
    const files = await Promise.all(
      [...event.dataTransfer.files].map(async (f) => ({ name: f.name, text: await f.text() })),
    );
    if (!files.length) return;
    const { imported } = await desktop.importData(files);
    if (imported.length) refresh();
  };

  const hosting = servers.some((s) => s.isHost);

  return (
    <div
      className={`home ${dragging ? "home--dragging" : ""}`}
      onDragOver={(e) => {
        if (e.dataTransfer.types.includes("Files")) {
          e.preventDefault();
          setDragging(true);
        }
      }}
      onDragLeave={(e) => {
        if (e.currentTarget === e.target) setDragging(false);
      }}
      onDrop={onDrop}
    >
      <RoughBox seed={7} radius={32} className="home__frame">
        <div className="home__scroll">
          <div className="home__toolbar">
            <button className="tool-button" onClick={() => setPanel({ kind: "host" })} title="Share canvases from this Mac">
              <RoughBox seed={21} radius={14} className="tool-button__box">
                {hosting && <span className="status-dot status-dot--on" />}
                {hosting ? "Hosting" : "Host"}
              </RoughBox>
            </button>
            <button className="tool-button" onClick={() => setPanel({ kind: "connect" })} title="Open canvases someone shared with you">
              <RoughBox seed={22} radius={14} className="tool-button__box">
                Connect…
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
          <h2 className="home__subtitle">Canvases</h2>
          {servers.length > 0 && <p className="home__section-meta">On this Mac</p>}

          <div className="home__grid">
            {creating ? (
              <RoughBox seed={11} className="card card--editing">
                <NameInput
                  placeholder="Name your canvas"
                  onSubmit={create}
                  onCancel={() => setCreating(false)}
                />
              </RoughBox>
            ) : (
              <NewCard seed={11} onClick={() => setCreating(true)} />
            )}

            {canvases?.map((canvas) =>
              renamingId === canvas.id ? (
                <RoughBox key={canvas.id} seed={seedFrom(canvas.id)} className="card card--editing">
                  <NameInput
                    initial={canvas.name}
                    onSubmit={(name) => rename(canvas.id, name)}
                    onCancel={() => setRenamingId(null)}
                  />
                </RoughBox>
              ) : (
                <CanvasCard
                  key={canvas.id}
                  canvas={canvas}
                  onOpen={() => onOpen(canvas.id)}
                  onMenu={() => showMenu(canvas)}
                />
              ),
            )}
          </div>

          {canvases?.length === 0 && !creating && (
            <p className="home__hint">
              No canvases yet. Create one, or drop <code>.excalidraw</code> files here to import them.
            </p>
          )}

          {servers.map((server) => (
            <ServerSection
              key={server.id}
              server={server}
              tick={remoteTick}
              onOpen={(canvasId) => onOpenRemote(server, canvasId)}
              onRemoved={refreshServers}
              onHostSettings={() => setPanel({ kind: "host" })}
            />
          ))}
        </div>
      </RoughBox>

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
      {panel?.kind === "host" && <HostPanel onClose={() => setPanel(null)} onChanged={refreshServers} />}
      {panel?.kind === "profile" && (
        <ProfilePanel profile={profile} onChange={onProfileChange} onClose={() => setPanel(null)} />
      )}
    </div>
  );
}

export function alertError(title: string, err: unknown) {
  desktop.showError(title, String((err as Error)?.message ?? err));
}

// ------------------------------------------------------------------ cards

function NewCard({ seed, onClick, label = "New canvas" }: { seed: number; onClick: () => void; label?: string }) {
  return (
    <button className="card-button" onClick={onClick}>
      <RoughBox seed={seed} dashed className="card card--new">
        <span className="card__plus">+</span>
        <span className="card__name">{label}</span>
      </RoughBox>
    </button>
  );
}

function CanvasCard({
  canvas,
  peers,
  onOpen,
  onMenu,
}: {
  canvas: CanvasMeta;
  peers?: Peer[];
  onOpen: () => void;
  onMenu: () => void;
}) {
  return (
    <button
      className="card-button"
      title={canvas.name}
      onClick={onOpen}
      onContextMenu={(e) => {
        e.preventDefault();
        onMenu();
      }}
    >
      <RoughBox seed={seedFrom(canvas.id)} className="card">
        <span className="card__name">{canvas.name}</span>
        <span className="card__meta">Edited {timeAgo(canvas.updatedAt)}</span>
        {peers && peers.length > 0 && <PeerStack peers={peers} />}
        <span
          role="button"
          tabIndex={-1}
          className="card__more"
          aria-label="More actions"
          onClick={(e) => {
            e.stopPropagation();
            onMenu();
          }}
        >
          •••
        </span>
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

// ------------------------------------------------------------------ server sections

function ServerSection({
  server,
  tick,
  onOpen,
  onRemoved,
  onHostSettings,
}: {
  server: ServerInfo;
  tick: number;
  onOpen: (canvasId: string) => void;
  onRemoved: () => void;
  onHostSettings: () => void;
}) {
  const [canvases, setCanvases] = useState<RemoteCanvas[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [creating, setCreating] = useState(false);
  const [renamingId, setRenamingId] = useState<string | null>(null);
  const [copied, setCopied] = useState(false);

  const load = useCallback(async () => {
    try {
      setCanvases(await desktop.remote.list(server.id));
      setError(null);
    } catch (err) {
      setError(String((err as Error)?.message ?? err));
    }
  }, [server.id]);

  useEffect(() => {
    load();
    const timer = setInterval(load, LIST_POLL);
    return () => clearInterval(timer);
  }, [load, tick]);

  const copyInvite = async (canvasId?: string) => {
    try {
      await desktop.copyText(await desktop.servers.inviteLink(server.id, canvasId));
    } catch (err) {
      return alertError("Couldn’t copy the invite link", err);
    }
    if (!canvasId) {
      setCopied(true);
      window.setTimeout(() => setCopied(false), 1600);
    }
  };

  const create = async (name: string) => {
    setCreating(false);
    try {
      const meta = await desktop.remote.create(server.id, name.trim() || "Untitled");
      onOpen(meta.id);
    } catch (err) {
      alertError("Couldn’t create the canvas", err);
    }
  };

  const rename = async (id: string, name: string) => {
    setRenamingId(null);
    if (name.trim()) {
      await desktop.remote.rename(server.id, id, name).catch((err) => alertError("Couldn’t rename", err));
    }
    load();
  };

  const showCanvasMenu = async (canvas: RemoteCanvas) => {
    const action = await desktop.contextMenu([
      { id: "open", text: "Open" },
      "separator",
      { id: "rename", text: "Rename…" },
      { id: "duplicate", text: "Duplicate" },
      { id: "invite", text: "Copy Invite Link" },
      { id: "export", text: "Export…" },
      "separator",
      { id: "delete", text: "Delete…" },
    ]);
    try {
      switch (action) {
        case "open":
          return onOpen(canvas.id);
        case "rename":
          return setRenamingId(canvas.id);
        case "duplicate":
          await desktop.remote.duplicate(server.id, canvas.id);
          return load();
        case "invite":
          return copyInvite(canvas.id);
        case "export":
          return await desktop.remote.exportCanvas(server.id, canvas.id, canvas.name);
        case "delete":
          if (await desktop.remote.remove(server.id, canvas.id, canvas.name)) load();
          return;
      }
    } catch (err) {
      alertError("Something went wrong", err);
    }
  };

  const showServerMenu = async () => {
    const action = await desktop.contextMenu(
      server.isHost
        ? [
            { id: "invite", text: "Copy Invite Link" },
            { id: "settings", text: "Hosting Settings…" },
          ]
        : [
            { id: "invite", text: "Copy Invite Link" },
            "separator",
            { id: "remove", text: "Remove Server" },
          ],
    );
    if (action === "invite") copyInvite();
    if (action === "settings") onHostSettings();
    if (action === "remove") {
      await desktop.servers.remove(server.id);
      onRemoved();
    }
  };

  const seedBase = seedFrom(server.id);

  return (
    <section className="server">
      <div className="server__header">
        <h2 className="home__subtitle server__title">{server.name}</h2>
        <button className="link-button" onClick={() => copyInvite()}>
          {copied ? "Copied!" : "Copy invite link"}
        </button>
        <button className="link-button link-button--quiet" aria-label="Server actions" onClick={showServerMenu}>
          •••
        </button>
      </div>
      <p className="home__section-meta">
        <span className={`status-dot ${error ? "status-dot--off" : "status-dot--on"}`} />
        {[server.isHost && "Hosted on this Mac", server.url.replace(/^https?:\/\//, "")].filter(Boolean).join(" · ")}
      </p>

      {error ? (
        <p className="server__error">
          {error}{" "}
          <button className="link-button" onClick={load}>
            Try again
          </button>
        </p>
      ) : (
        <div className="home__grid">
          {creating ? (
            <RoughBox seed={seedBase + 1} className="card card--editing">
              <NameInput placeholder="Name your canvas" onSubmit={create} onCancel={() => setCreating(false)} />
            </RoughBox>
          ) : (
            <NewCard seed={seedBase + 1} onClick={() => setCreating(true)} />
          )}
          {canvases?.map((canvas) =>
            renamingId === canvas.id ? (
              <RoughBox key={canvas.id} seed={seedFrom(canvas.id)} className="card card--editing">
                <NameInput
                  initial={canvas.name}
                  onSubmit={(name) => rename(canvas.id, name)}
                  onCancel={() => setRenamingId(null)}
                />
              </RoughBox>
            ) : (
              <CanvasCard
                key={canvas.id}
                canvas={canvas}
                peers={canvas.peers}
                onOpen={() => onOpen(canvas.id)}
                onMenu={() => showCanvasMenu(canvas)}
              />
            ),
          )}
        </div>
      )}
    </section>
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
