// In-app replacements for native popups: context menus, confirmations and
// error alerts, so everything matches the app's own look.
import { useEffect, useLayoutEffect, useRef, useState, useSyncExternalStore } from "react";
import { RoughBox } from "./RoughBox";

export type MenuEntry =
  | { id: string; text: string; enabled?: boolean }
  | { text: string; items: MenuEntry[]; enabled?: boolean }
  | "separator";

export type Anchor = { x: number; y: number };

/** Where a menu should open for a mouse event: the pointer for right-clicks, the button's corner for clicks. */
export function anchorOf(event: { clientX: number; clientY: number; type: string; currentTarget: EventTarget }): Anchor {
  if (event.type === "contextmenu") return { x: event.clientX, y: event.clientY };
  const rect = (event.currentTarget as HTMLElement).getBoundingClientRect();
  return { x: rect.right, y: rect.bottom + 4 };
}

type Layer =
  | { kind: "menu"; entries: MenuEntry[]; anchor: Anchor; resolve: (id: string | null) => void }
  | {
      kind: "dialog";
      title: string;
      message: string;
      confirmLabel: string | null; // null → plain alert
      danger: boolean;
      resolve: (ok: boolean) => void;
    };

let layer: Layer | null = null;
const listeners = new Set<() => void>();

function setLayer(next: Layer | null) {
  layer = next;
  listeners.forEach((l) => l());
}

function dismiss() {
  if (!layer) return;
  const current = layer;
  setLayer(null);
  if (current.kind === "menu") current.resolve(null);
  else current.resolve(false);
}

export const ui = {
  /** Opens a context menu; resolves with the chosen id, or null if dismissed. */
  menu(entries: MenuEntry[], anchor: Anchor): Promise<string | null> {
    dismiss();
    return new Promise((resolve) => setLayer({ kind: "menu", entries, anchor, resolve }));
  },
  confirm(opts: { title: string; message: string; confirmLabel: string; danger?: boolean }): Promise<boolean> {
    dismiss();
    return new Promise((resolve) =>
      setLayer({ kind: "dialog", title: opts.title, message: opts.message, confirmLabel: opts.confirmLabel, danger: !!opts.danger, resolve }),
    );
  },
  alert(title: string, message: string): Promise<void> {
    dismiss();
    return new Promise((resolve) =>
      setLayer({ kind: "dialog", title, message, confirmLabel: null, danger: false, resolve: () => resolve() }),
    );
  },
};

export function UiHost() {
  const current = useSyncExternalStore(
    (cb) => (listeners.add(cb), () => listeners.delete(cb)),
    () => layer,
  );
  if (!current) return null;
  return current.kind === "menu" ? <MenuLayer layer={current} /> : <DialogLayer layer={current} />;
}

function close(result: string | boolean | null, current: Layer) {
  setLayer(null);
  if (current.kind === "menu") current.resolve(result as string | null);
  else current.resolve(result === true);
}

// ------------------------------------------------------------------ menu

function MenuLayer({ layer: current }: { layer: Extract<Layer, { kind: "menu" }> }) {
  const ref = useRef<HTMLDivElement>(null);
  const [pos, setPos] = useState<Anchor & { ready: boolean }>({ ...current.anchor, ready: false });

  // Anchor the menu's top-right corner to the point when it's a button, else its top-left; keep it on screen.
  useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    const { width, height } = el.getBoundingClientRect();
    const margin = 8;
    let x = current.anchor.x;
    let y = current.anchor.y;
    if (x + width > window.innerWidth - margin) x = Math.max(margin, x - width);
    if (y + height > window.innerHeight - margin) y = Math.max(margin, window.innerHeight - height - margin);
    setPos({ x, y, ready: true });
  }, [current]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && close(null, current);
    const onBlur = () => close(null, current);
    window.addEventListener("keydown", onKey);
    window.addEventListener("blur", onBlur);
    window.addEventListener("resize", onBlur);
    return () => {
      window.removeEventListener("keydown", onKey);
      window.removeEventListener("blur", onBlur);
      window.removeEventListener("resize", onBlur);
    };
  }, [current]);

  return (
    <div
      className="ui-scrim"
      onMouseDown={(e) => e.target === e.currentTarget && close(null, current)}
      onContextMenu={(e) => {
        e.preventDefault();
        if (e.target === e.currentTarget) close(null, current);
      }}
    >
      <div
        ref={ref}
        className="ui-menu"
        role="menu"
        style={{ left: pos.x, top: pos.y, visibility: pos.ready ? "visible" : "hidden" }}
      >
        <MenuItems entries={current.entries} onPick={(id) => close(id, current)} />
      </div>
    </div>
  );
}

function MenuItems({ entries, onPick }: { entries: MenuEntry[]; onPick: (id: string) => void }) {
  return (
    <>
      {entries.map((entry, i) => {
        if (entry === "separator") return <div key={i} className="ui-menu__separator" role="separator" />;
        const disabled = entry.enabled === false;
        if ("items" in entry) return <SubMenu key={i} entry={entry} disabled={disabled} onPick={onPick} />;
        return (
          <button
            key={entry.id}
            role="menuitem"
            className={`ui-menu__item${entry.id === "delete" || entry.id === "remove" ? " ui-menu__item--danger" : ""}`}
            disabled={disabled}
            onClick={() => onPick(entry.id)}
          >
            {entry.text}
          </button>
        );
      })}
    </>
  );
}

function SubMenu({
  entry,
  disabled,
  onPick,
}: {
  entry: { text: string; items: MenuEntry[] };
  disabled: boolean;
  onPick: (id: string) => void;
}) {
  const [open, setOpen] = useState(false);
  const ref = useRef<HTMLDivElement>(null);
  const [flip, setFlip] = useState(false);

  useLayoutEffect(() => {
    if (!open || !ref.current) return;
    const rect = ref.current.getBoundingClientRect();
    setFlip(rect.right > window.innerWidth - 8);
  }, [open]);

  return (
    <div className="ui-menu__sub" onMouseEnter={() => !disabled && setOpen(true)} onMouseLeave={() => setOpen(false)}>
      <button
        role="menuitem"
        aria-haspopup="menu"
        className="ui-menu__item ui-menu__item--parent"
        disabled={disabled}
        onClick={() => setOpen((o) => !o)}
      >
        {entry.text}
        <span className="ui-menu__chevron">›</span>
      </button>
      {open && (
        <div ref={ref} className={`ui-menu ui-menu--sub${flip ? " ui-menu--flip" : ""}`} role="menu">
          <MenuItems entries={entry.items} onPick={onPick} />
        </div>
      )}
    </div>
  );
}

// ------------------------------------------------------------------ dialog

function DialogLayer({ layer: current }: { layer: Extract<Layer, { kind: "dialog" }> }) {
  const confirmRef = useRef<HTMLButtonElement>(null);
  const isConfirm = current.confirmLabel !== null;

  useEffect(() => {
    confirmRef.current?.focus();
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") close(false, current);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [current]);

  return (
    <div className="modal" onMouseDown={(e) => e.target === e.currentTarget && close(false, current)}>
      <RoughBox seed={47} radius={24} className="modal__box ui-dialog">
        <div className="modal__body" role="alertdialog" aria-label={current.title}>
          <h2 className="modal__title ui-dialog__title">{current.title}</h2>
          <p className="form__help ui-dialog__message">{current.message}</p>
          <div className="form__actions">
            {isConfirm && (
              <button type="button" className="text-button" onClick={() => close(false, current)}>
                Cancel
              </button>
            )}
            <button
              ref={confirmRef}
              type="button"
              className={`text-button ${current.danger ? "text-button--solid-danger" : "text-button--primary"}`}
              onClick={() => close(true, current)}
            >
              {current.confirmLabel ?? "OK"}
            </button>
          </div>
        </div>
      </RoughBox>
    </div>
  );
}
