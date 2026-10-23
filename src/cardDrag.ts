// Dragging canvas cards on the Welcome screen: to rearrange them, to move them
// to another space, or to the trash. Pointer events rather than HTML5 drag and
// drop, which stays free for importing files dropped from Finder.
import { useCallback, useEffect, useRef, useState, type PointerEvent as ReactPointerEvent, type RefObject } from "react";

/**
 * Where a card would land. `index` is its position among the cards shown in
 * the space (null: at the front). A folder target files it in that folder
 * (null: out of any folder).
 */
export type DropTarget =
  | { kind: "space"; space: string; index: number | null; dock?: boolean }
  | { kind: "folder"; space: string; folder: string | null }
  | { kind: "trash" };

export type CardDrag<T> = {
  item: T;
  from: string;
  /** Pointer position, and where on the card it was grabbed. */
  x: number;
  y: number;
  grabX: number;
  grabY: number;
  width: number;
  height: number;
  over: DropTarget | null;
};

/** How far the pointer moves before a press becomes a drag (a shorter press is a click). */
const THRESHOLD = 6;
/** Distance from the top or bottom of the list at which it starts scrolling, and the top speed. */
const EDGE = 70;
const MAX_SCROLL = 18;

/** Scroll speed for a pointer at `y`: faster the closer it is to the top or bottom edge of `box`. */
export function edgeSpeed(y: number, box: DOMRect) {
  if (y < box.top + EDGE) return -MAX_SCROLL * (1 - Math.max(0, y - box.top) / EDGE);
  if (y > box.bottom - EDGE) return MAX_SCROLL * (1 - Math.max(0, box.bottom - y) / EDGE);
  return 0;
}

/**
 * Drop targets are marked in the DOM: `data-drop="space"` with `data-space`
 * (and an optional `data-grid` of equally sized cells inside, whose first
 * `data-lead` cells, "New canvas" and folders, aren't canvases),
 * `data-drop="folder"` with `data-space` and `data-folder` (empty: out of any
 * folder), or `data-drop="trash"`. Dock targets also have `data-dock`.
 */
function hitTest(x: number, y: number): DropTarget | null {
  for (const el of document.elementsFromPoint(x, y)) {
    const target = (el as HTMLElement).closest<HTMLElement>("[data-drop]");
    if (!target) continue;
    if (target.dataset.drop === "trash") return { kind: "trash" };
    const space = target.dataset.space!;
    if (target.dataset.drop === "folder") return { kind: "folder", space, folder: target.dataset.folder || null };
    if (target.dataset.dock !== undefined) return { kind: "space", space, index: null, dock: true };
    const grid = target.querySelector<HTMLElement>("[data-grid]");
    const rect = grid?.getBoundingClientRect();
    if (grid && rect && y > rect.bottom) return { kind: "space", space, index: Number.MAX_SAFE_INTEGER };
    if (grid && rect && y >= rect.top) return { kind: "space", space, index: cellAt(grid, x, y) };
    return { kind: "space", space, index: null };
  }
  return null;
}

/** Card position under the point, from the grid's geometry (stable while cards shift around). */
function cellAt(grid: HTMLElement, x: number, y: number) {
  const first = grid.firstElementChild?.getBoundingClientRect();
  if (!first) return 0;
  const style = getComputedStyle(grid);
  const cols = style.gridTemplateColumns.split(" ").length;
  const colGap = parseFloat(style.columnGap) || 0;
  const rowGap = parseFloat(style.rowGap) || 0;
  const rect = grid.getBoundingClientRect();
  const col = Math.min(cols - 1, Math.max(0, Math.floor((x - rect.left + colGap / 2) / (first.width + colGap))));
  const row = Math.max(0, Math.floor((y - rect.top + rowGap / 2) / (first.height + rowGap)));
  return Math.max(0, row * cols + col - Number(grid.dataset.lead ?? 1));
}

export function useCardDrag<T>(scroller: RefObject<HTMLElement | null>, onDrop: (item: T, from: string, target: DropTarget) => void) {
  const [drag, setDrag] = useState<CardDrag<T> | null>(null);
  const live = useRef<CardDrag<T> | null>(null);
  const onDropRef = useRef(onDrop);
  onDropRef.current = onDrop;
  const cleanup = useRef<(() => void) | null>(null);

  useEffect(() => () => cleanup.current?.(), []);

  const start = useCallback(
    (event: ReactPointerEvent<HTMLElement>, item: T, from: string) => {
      if (event.button !== 0 || cleanup.current) return;
      const card = event.currentTarget.getBoundingClientRect();
      const startX = event.clientX;
      const startY = event.clientY;
      let frame = 0;

      const update = (x: number, y: number) => {
        const next = { ...live.current!, x, y, over: hitTest(x, y) };
        live.current = next;
        setDrag(next);
      };

      // Near the top or bottom edge, keep scrolling so far-away spaces are reachable.
      const autoScroll = () => {
        frame = requestAnimationFrame(autoScroll);
        const el = scroller.current;
        const d = live.current;
        // The dock sits in the bottom edge: reaching for it shouldn't scroll.
        if (!el || !d || d.over?.kind === "trash" || (d.over?.kind === "space" && d.over.dock)) return;
        const speed = edgeSpeed(d.y, el.getBoundingClientRect());
        if (speed) {
          el.scrollTop += speed;
          update(d.x, d.y);
        }
      };

      const onMove = (e: PointerEvent) => {
        if (!live.current) {
          if (Math.hypot(e.clientX - startX, e.clientY - startY) < THRESHOLD) return;
          live.current = {
            item,
            from,
            x: e.clientX,
            y: e.clientY,
            grabX: startX - card.left,
            grabY: startY - card.top,
            width: card.width,
            height: card.height,
            over: null,
          };
          frame = requestAnimationFrame(autoScroll);
        }
        update(e.clientX, e.clientY);
      };

      const finish = (drop: boolean) => {
        const d = live.current;
        stop();
        if (!d) return;
        // The press ended a drag, not a click: don't let it open the canvas.
        const swallow = (e: MouseEvent) => {
          e.stopPropagation();
          e.preventDefault();
        };
        window.addEventListener("click", swallow, { capture: true, once: true });
        setTimeout(() => window.removeEventListener("click", swallow, { capture: true }), 0);
        if (drop && d.over) onDropRef.current(d.item, d.from, d.over);
      };

      const onUp = () => finish(true);
      const onKey = (e: KeyboardEvent) => e.key === "Escape" && finish(false);
      const onBlur = () => finish(false);

      const stop = () => {
        cancelAnimationFrame(frame);
        window.removeEventListener("pointermove", onMove);
        window.removeEventListener("pointerup", onUp);
        window.removeEventListener("pointercancel", onBlur);
        window.removeEventListener("keydown", onKey);
        window.removeEventListener("blur", onBlur);
        live.current = null;
        cleanup.current = null;
        setDrag(null);
      };

      window.addEventListener("pointermove", onMove);
      window.addEventListener("pointerup", onUp);
      window.addEventListener("pointercancel", onBlur);
      window.addEventListener("keydown", onKey);
      window.addEventListener("blur", onBlur);
      cleanup.current = stop;
    },
    [scroller],
  );

  return { drag, start };
}
