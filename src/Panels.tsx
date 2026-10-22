// Small dialogs: connect to a server, host from this Mac, connect an AI
// assistant, and how you appear to others.
import { useCallback, useEffect, useRef, useState, type ReactNode } from "react";
import {
  desktop,
  parseInvite,
  type AiConnectInfo,
  type AiSnippets,
  type HostStatus,
  type Profile,
  type RecentServer,
  type ServerInfo,
} from "./desktop";
import { RoughBox } from "./RoughBox";
import { DrawnIcon } from "./icons";
import { clientColor, initials } from "./colors";

export type Panel =
  | { kind: "connect"; link?: string; auto?: boolean }
  | { kind: "host" }
  | { kind: "ai"; serverId: string; canvasId?: string; canvasName?: string }
  | { kind: "profile" };

function Modal({ title, onClose, children }: { title: string; onClose: () => void; children: ReactNode }) {
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && onClose();
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  return (
    <div className="modal" onMouseDown={(e) => e.target === e.currentTarget && onClose()}>
      <RoughBox seed={31} radius={24} className="modal__box">
        <div className="modal__body" role="dialog" aria-label={title}>
          <h2 className="modal__title">{title}</h2>
          {children}
        </div>
      </RoughBox>
    </div>
  );
}

const message = (err: unknown) => String((err as Error)?.message ?? err);

// ------------------------------------------------------------------ connect

export function ConnectPanel({
  initialLink = "",
  autoSubmit = false,
  onClose,
  onConnected,
}: {
  initialLink?: string;
  autoSubmit?: boolean;
  onClose: () => void;
  onConnected: (server: ServerInfo, canvasId: string | null) => void;
}) {
  const [address, setAddress] = useState(initialLink);
  const [token, setToken] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [recent, setRecent] = useState<RecentServer[]>([]);
  const invite = parseInvite(address);
  const needsToken = address.trim() !== "" && !invite && !address.trim().startsWith("drawlycool:");

  useEffect(() => {
    desktop.servers.recent().then(setRecent);
  }, []);

  const run = async (join: () => Promise<ServerInfo>, canvasId: string | null = null) => {
    if (busy) return;
    setError(null);
    setBusy(true);
    try {
      onConnected(await join(), canvasId);
    } catch (err) {
      setError(message(err));
    } finally {
      setBusy(false);
    }
  };

  const connect = (link = address) => {
    const parsed = parseInvite(link);
    if (link.trim().startsWith("drawlycool:") && !parsed) {
      setError("That invite link is incomplete. Ask for a new one.");
      return;
    }
    run(() => (parsed ? desktop.servers.add(parsed.server, parsed.token) : desktop.servers.add(link, token)), parsed?.canvas ?? null);
  };

  const autoRan = useRef(false);
  useEffect(() => {
    if (autoSubmit && !autoRan.current && initialLink) {
      autoRan.current = true;
      connect();
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  return (
    <Modal title="Join friends" onClose={onClose}>
      <form
        className="form"
        onSubmit={(e) => {
          e.preventDefault();
          connect();
        }}
      >
        <input
          className="form__input form__input--big"
          autoFocus
          spellCheck={false}
          aria-label="Invite link"
          placeholder="Paste their invite link"
          value={address}
          onChange={(e) => setAddress(e.target.value)}
          // A pasted invite is all it takes.
          onPaste={(e) => {
            const text = e.clipboardData.getData("text").trim();
            if (!parseInvite(text)) return;
            e.preventDefault();
            setAddress(text);
            connect(text);
          }}
        />
        {needsToken && (
          <label className="form__label">
            Access token
            <input
              className="form__input"
              spellCheck={false}
              value={token}
              onChange={(e) => setToken(e.target.value)}
            />
          </label>
        )}
        {!address && <p className="form__help">No link yet? Ask whoever’s sharing to send you one.</p>}
        {error && <p className="form__error">{error}</p>}

        {recent.length > 0 && (
          <div className="form__label">
            Play again with
            <div className="chips">
              {recent.map((r) => (
                <span key={r.url} className="chip">
                  <button type="button" className="chip__main" title={r.url} disabled={busy} onClick={() => run(() => desktop.servers.rejoin(r.url))}>
                    <DrawnIcon name="cloud" size={18} />
                    {r.name}
                  </button>
                  <button
                    type="button"
                    className="chip__forget"
                    title="Forget"
                    aria-label={`Forget ${r.name}`}
                    onClick={async () => {
                      await desktop.servers.forget(r.url);
                      setRecent((list) => list.filter((x) => x.url !== r.url));
                    }}
                  >
                    <DrawnIcon name="close" size={12} />
                  </button>
                </span>
              ))}
            </div>
          </div>
        )}

        <div className="form__actions">
          <button type="button" className="text-button" onClick={onClose}>
            Cancel
          </button>
          <button type="submit" className="text-button text-button--primary" disabled={busy || !address.trim()}>
            {busy ? "Joining…" : "Join"}
          </button>
        </div>
      </form>
    </Modal>
  );
}

// ------------------------------------------------------------------ host

export function HostPanel({ onClose, onChanged }: { onClose: () => void; onChanged: () => void }) {
  const [status, setStatus] = useState<HostStatus | null>(null);
  const [busy, setBusy] = useState(false);
  const [copied, setCopied] = useState(false);
  const [confirmReset, setConfirmReset] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    desktop.host.status().then(setStatus);
  }, []);

  // Watch for cloudflared being installed and for the tunnel coming up.
  const tunnelState = status?.enabled && status.running && status.internet ? status.tunnel.state : "off";
  useEffect(() => {
    if (tunnelState === "ready") onChanged();
    if (tunnelState !== "missing" && tunnelState !== "starting") return;
    const timer = window.setInterval(() => desktop.host.status().then(setStatus), 1500);
    return () => window.clearInterval(timer);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [tunnelState]);

  const update = async (patch: Parameters<typeof desktop.host.update>[0]) => {
    setBusy(true);
    setError(null);
    try {
      setStatus(await desktop.host.update(patch));
      onChanged();
    } catch (err) {
      setError(message(err));
    } finally {
      setBusy(false);
    }
  };

  const copyInvite = async () => {
    await desktop.copyText(await desktop.servers.inviteLink("this-mac"));
    setCopied(true);
    window.setTimeout(() => setCopied(false), 1600);
  };

  if (!status) return null;
  const on = status.enabled;
  const shownError = error ?? status.error;
  const selectedAddress = status.inviteAddress ?? status.addresses[0]?.address ?? "";

  return (
    <Modal title="Sharing" onClose={onClose}>
      <div className="form">
        <label className="switch">
          <input
            type="checkbox"
            checked={on}
            disabled={busy}
            onChange={(e) => update({ enabled: e.target.checked })}
          />
          <span className="switch__track" />
          <span>Share “With friends” from this Mac</span>
        </label>

        <p className="form__help">Friends with the invite link can draw there while this Mac is awake and Drawlycool is open.</p>

        {on && status.running && (
          <div className="form__label">
            Who can join
            <div className="segmented" role="group">
              {[
                { internet: true, label: "Anywhere" },
                { internet: false, label: "Same Wi-Fi or VPN" },
              ].map((o) => (
                <button
                  key={o.label}
                  type="button"
                  className={`segmented__option${status.internet === o.internet ? " segmented__option--on" : ""}`}
                  aria-pressed={status.internet === o.internet}
                  disabled={busy}
                  onClick={() => status.internet !== o.internet && update({ internet: o.internet })}
                >
                  {o.label}
                </button>
              ))}
            </div>
          </div>
        )}

        {on && status.running && status.internet && (
          <InternetInvite status={status} busy={busy} copied={copied} onCopy={copyInvite} onRetry={() => update({ internet: true })} />
        )}

        {on && status.running && !status.internet && (
          <>
            <label className="form__label">
              Invite people with
              <select
                className="form__input"
                value={selectedAddress}
                disabled={busy}
                onChange={(e) => update({ inviteAddress: e.target.value })}
              >
                {status.addresses.map((a) => (
                  <option key={a.address} value={a.address}>
                    {a.address} — {a.label}
                  </option>
                ))}
                {!status.addresses.some((a) => a.address === selectedAddress) && selectedAddress && (
                  <option value={selectedAddress}>{selectedAddress}</option>
                )}
              </select>
            </label>
            <div className="invite">
              <code className="invite__url">{status.inviteUrl}</code>
              <button className="text-button text-button--primary" onClick={copyInvite}>
                {copied ? "Copied!" : "Copy invite link"}
              </button>
            </div>
          </>
        )}

        {on && (
          <div className="form__row">
            <label className="form__label form__label--grow">
              Name friends see
              <input
                className="form__input"
                defaultValue={status.name}
                key={status.name}
                maxLength={60}
                disabled={busy}
                onBlur={(e) => e.target.value.trim() !== status.name && update({ name: e.target.value })}
                onKeyDown={(e) => e.key === "Enter" && e.currentTarget.blur()}
              />
            </label>
            <label className="form__label form__label--port">
              Port
              <input
                className="form__input"
                defaultValue={status.port}
                key={status.port}
                inputMode="numeric"
                disabled={busy}
                onBlur={(e) => {
                  const port = Number(e.target.value);
                  if (Number.isInteger(port) && port >= 1024 && port <= 65535 && port !== status.port) update({ port });
                  else e.target.value = String(status.port);
                }}
                onKeyDown={(e) => e.key === "Enter" && e.currentTarget.blur()}
              />
            </label>
          </div>
        )}

        {shownError && <p className="form__error">{shownError}</p>}

        <div className="form__actions">
          {on && (
            <button
              className="text-button text-button--danger"
              disabled={busy}
              onClick={() => {
                if (!confirmReset) return setConfirmReset(true);
                setConfirmReset(false);
                update({ resetToken: true });
              }}
              onBlur={() => setConfirmReset(false)}
            >
              {confirmReset ? "Click again: old invite links stop working" : "Reset access"}
            </button>
          )}
          <span className="form__spacer" />
          <button className="text-button text-button--primary" onClick={onClose}>
            Done
          </button>
        </div>
      </div>
    </Modal>
  );
}

/** The invite for "Anyone, over the internet": install steps until cloudflared is there, then the link. */
function InternetInvite({
  status,
  busy,
  copied,
  onCopy,
  onRetry,
}: {
  status: HostStatus;
  busy: boolean;
  copied: boolean;
  onCopy: () => void;
  onRetry: () => void;
}) {
  const [copiedCommand, setCopiedCommand] = useState(false);
  const tunnel = status.tunnel;

  if (tunnel.state === "missing") {
    const copyCommand = async () => {
      await desktop.copyText(status.installCommand);
      setCopiedCommand(true);
      window.setTimeout(() => setCopiedCommand(false), 1600);
    };
    return (
      <div className="setup">
        <p className="setup__title">One-time setup: paste this in Terminal</p>
        <p className="form__help">It installs cloudflared, Cloudflare’s free connector. No account needed.</p>
        <div className="invite">
          <code className="invite__url invite__url--wrap">{status.installCommand}</code>
          <button className="text-button text-button--primary" onClick={copyCommand}>
            {copiedCommand ? "Copied!" : "Copy"}
          </button>
        </div>
        <p className="form__help setup__waiting">Waiting… the invite link shows up by itself.</p>
      </div>
    );
  }

  if (tunnel.state === "failed") {
    return (
      <div className="setup">
        <p className="form__error">{tunnel.error}</p>
        <div>
          <button className="text-button text-button--primary" disabled={busy} onClick={onRetry}>
            Try again
          </button>
        </div>
      </div>
    );
  }

  const ready = tunnel.state === "ready";
  return (
    <>
      <div className="invite">
        <code className={`invite__url${ready ? "" : " invite__url--pending"}`}>
          {ready ? status.inviteUrl.replace(/^https:\/\//, "") : "Getting a link from Cloudflare…"}
        </code>
        <button className="text-button text-button--primary" disabled={!ready} onClick={onCopy}>
          {copied ? "Copied!" : "Copy invite link"}
        </button>
      </div>
      <p className="form__help">The link changes when Drawlycool restarts.</p>
    </>
  );
}

// ------------------------------------------------------------------ AI assistants

type AiClient = "claude-code" | "claude-cloud" | "other";

const AI_CLIENTS: { id: AiClient; label: string }[] = [
  { id: "claude-code", label: "Claude Code" },
  { id: "claude-cloud", label: "Claude Desktop & claude.ai" },
  { id: "other", label: "Other apps" },
];

/** Settings that let an AI assistant join a server (or one canvas) over MCP and draw live. */
export function AiConnectPanel({
  serverId,
  canvasId,
  canvasName,
  onClose,
  onHostSettings,
}: {
  serverId: string;
  canvasId?: string;
  canvasName?: string;
  onClose: () => void;
  /** Shown when cloud apps need hosting changes first. */
  onHostSettings?: () => void;
}) {
  const [info, setInfo] = useState<AiConnectInfo | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [client, setClient] = useState<AiClient>("claude-code");

  const load = useCallback(async () => {
    try {
      setInfo(await desktop.servers.aiConnectInfo(serverId, canvasId, canvasName));
      setError(null);
    } catch (err) {
      setError(message(err));
    }
  }, [serverId, canvasId, canvasName]);

  useEffect(() => {
    load();
  }, [load]);

  // Watch for the internet link coming up (or cloudflared being installed).
  useEffect(() => {
    if (!info?.waiting) return;
    const timer = window.setInterval(load, 1500);
    return () => window.clearInterval(timer);
  }, [info?.waiting, load]);

  const title = "Connect an AI assistant";
  if (!info) {
    return (
      <Modal title={title} onClose={onClose}>
        <div className="form">
          {error ? <p className="form__error">{error}</p> : <p className="form__help">Loading…</p>}
          <div className="form__actions">
            <button className="text-button text-button--primary" onClick={onClose}>
              Done
            </button>
          </div>
        </div>
      </Modal>
    );
  }

  // Assistants on this Mac use the local address; everything else the remote one.
  const nearby = (info.local ?? info.remote) as AiSnippets;
  const cloud = info.remote && !info.cloudNote ? info.remote : null;
  const where = info.canvasName ? (
    <>
      It starts in <em>{info.canvasName}</em>.
    </>
  ) : (
    <>
      It can open any canvas on <em>{info.serverName}</em>; just ask.
    </>
  );

  return (
    <Modal title={title} onClose={onClose}>
      <div className="form">
        <p className="form__help">
          Lets Claude or another AI assistant join {info.canvasId ? "this canvas" : "your canvases"} and draw with you
          live; it shows up with its own cursor. {where}
        </p>

        <div className="segmented segmented--fit" role="group" aria-label="AI assistant">
          {AI_CLIENTS.map((c) => (
            <button
              key={c.id}
              type="button"
              className={`segmented__option${client === c.id ? " segmented__option--on" : ""}`}
              aria-pressed={client === c.id}
              onClick={() => setClient(c.id)}
            >
              {c.label}
            </button>
          ))}
        </div>

        {client === "claude-code" && (
          <>
            <CodeBox text={nearby.claudeCode} />
            <p className="form__help">
              Run this in Terminal, then start <code>claude</code> and ask it to draw something.
            </p>
          </>
        )}

        {client === "claude-cloud" &&
          (cloud ? (
            <>
              <CodeBox text={cloud.connectorUrl} />
              <p className="form__help">
                In Claude Desktop or claude.ai, open Settings → Connectors → <em>Add custom connector</em> and paste this
                URL. Then ask Claude to draw something.
              </p>
            </>
          ) : (
            <>
              <div className="setup">
                <p className="setup__title">{info.cloudNote}</p>
                {info.waiting && (
                  <p className="form__help setup__waiting">Waiting for the internet link… the URL shows up here by itself.</p>
                )}
                {serverId === "this-mac" && onHostSettings && (
                  <div>
                    <button className="text-button text-button--primary" onClick={onHostSettings}>
                      Open Host settings
                    </button>
                  </div>
                )}
              </div>
              <p className="form__help">
                Meanwhile, Claude Desktop can connect through a local bridge (needs Node.js): open Settings → Developer →{" "}
                <em>Edit Config</em>, add this to <code>claude_desktop_config.json</code>, and restart Claude.
              </p>
              <CodeBox text={nearby.bridgeConfig} block />
            </>
          ))}

        {client === "other" && (
          <>
            <p className="form__help">For apps that connect to MCP servers over HTTP, like Cursor or Windsurf:</p>
            <CodeBox text={nearby.jsonConfig} block />
            <p className="form__help">For apps that only run local commands (needs Node.js):</p>
            <CodeBox text={nearby.bridgeConfig} block />
            {info.local && info.remote && (
              <p className="form__help">
                On another computer, use <code>{info.remote.url}</code> as the URL instead.
              </p>
            )}
          </>
        )}

        <p className="form__help">
          These contain this server’s access token: anyone with it can edit every canvas here. Share it like a password.
        </p>
        {error && <p className="form__error">{error}</p>}

        <div className="form__actions">
          <button className="text-button text-button--primary" onClick={onClose}>
            Done
          </button>
        </div>
      </div>
    </Modal>
  );
}

/** A command or config to paste somewhere, with a Copy button. */
function CodeBox({ text, block = false }: { text: string; block?: boolean }) {
  const [copied, setCopied] = useState(false);
  const copy = async () => {
    await desktop.copyText(text);
    setCopied(true);
    window.setTimeout(() => setCopied(false), 1600);
  };
  return (
    <div className={`invite${block ? " invite--block" : ""}`}>
      <code className={`invite__url ${block ? "invite__url--code" : "invite__url--wrap"}`}>{text}</code>
      <button className="text-button text-button--primary" onClick={copy}>
        {copied ? "Copied!" : "Copy"}
      </button>
    </div>
  );
}

// ------------------------------------------------------------------ profile

export function ProfilePanel({
  profile,
  onChange,
  onClose,
}: {
  profile: Profile;
  onChange: (profile: Profile) => void;
  onClose: () => void;
}) {
  const [name, setName] = useState(profile.name);

  const save = () => {
    onChange({ ...profile, name: name.trim() || profile.name });
    onClose();
  };

  return (
    <Modal title="How others see you" onClose={onClose}>
      <form
        className="form"
        onSubmit={(e) => {
          e.preventDefault();
          save();
        }}
      >
        <div className="profile-preview">
          <span className="avatar" style={{ background: clientColor(profile.id) }}>
            {initials(name || profile.name)}
          </span>
          <span className="profile-preview__tag" style={{ background: clientColor(profile.id) }}>
            {name.trim() || profile.name}
          </span>
        </div>
        <label className="form__label">
          Name
          <input
            className="form__input"
            autoFocus
            maxLength={40}
            value={name}
            onChange={(e) => setName(e.target.value)}
          />
        </label>
        <p className="form__help">
          Shown next to your cursor on shared canvases. Changes apply the next time you open one.
        </p>
        <div className="form__actions">
          <button type="button" className="text-button" onClick={onClose}>
            Cancel
          </button>
          <button type="submit" className="text-button text-button--primary">
            Save
          </button>
        </div>
      </form>
    </Modal>
  );
}
