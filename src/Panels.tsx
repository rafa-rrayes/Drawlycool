// Small dialogs on the Welcome screen: connect to a server, host from this Mac,
// and how you appear to others.
import { useEffect, useRef, useState, type ReactNode } from "react";
import { desktop, parseInvite, type HostStatus, type Profile, type ServerInfo } from "./desktop";
import { RoughBox } from "./RoughBox";
import { clientColor, initials } from "./colors";

export type Panel =
  | { kind: "connect"; link?: string; auto?: boolean }
  | { kind: "host" }
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
  const invite = parseInvite(address);
  const needsToken = address.trim() !== "" && !invite && !address.trim().startsWith("drawlycool:");

  const connect = async () => {
    if (busy) return;
    setError(null);
    if (address.trim().startsWith("drawlycool:") && !invite) {
      setError("That invite link is incomplete. Ask for a new one.");
      return;
    }
    setBusy(true);
    try {
      const server = invite
        ? await desktop.servers.add(invite.server, invite.token)
        : await desktop.servers.add(address, token);
      onConnected(server, invite?.canvas ?? null);
    } catch (err) {
      setError(message(err));
    } finally {
      setBusy(false);
    }
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
    <Modal title="Connect to a server" onClose={onClose}>
      <form
        className="form"
        onSubmit={(e) => {
          e.preventDefault();
          connect();
        }}
      >
        <label className="form__label">
          Invite link or server address
          <input
            className="form__input"
            autoFocus
            spellCheck={false}
            placeholder="drawlycool://connect?server=…"
            value={address}
            onChange={(e) => setAddress(e.target.value)}
          />
        </label>
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
        <p className="form__help">
          Ask whoever hosts the canvases for an invite link. Hosts copy it from <em>Host</em> on their Welcome screen,
          or from any shared canvas.
        </p>
        {error && <p className="form__error">{error}</p>}
        <div className="form__actions">
          <button type="button" className="text-button" onClick={onClose}>
            Cancel
          </button>
          <button type="submit" className="text-button text-button--primary" disabled={busy || !address.trim()}>
            {busy ? "Connecting…" : "Connect"}
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
    <Modal title="Host on this Mac" onClose={onClose}>
      <div className="form">
        <label className="switch">
          <input
            type="checkbox"
            checked={on}
            disabled={busy}
            onChange={(e) => update({ enabled: e.target.checked })}
          />
          <span className="switch__track" />
          <span>Share canvases from this Mac</span>
        </label>

        <p className="form__help">
          Canvases you create under <em>{status.name || "this Mac"}</em> on the Welcome screen can be opened and
          edited live by anyone with the invite link, while this Mac is awake and Drawlycool is open. Works on the same
          network, or from anywhere over Tailscale or a VPN.
        </p>

        {on && status.running && (
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
              Name others see
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
