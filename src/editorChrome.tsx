// Pieces shared by the local and remote canvas editors.
import { useCallback, useEffect, useRef, type ReactNode, type RefObject } from "react";
import { CaptureUpdateAction, MainMenu } from "@excalidraw/excalidraw";
import type { AppState, ExcalidrawImperativeAPI, UIOptions } from "@excalidraw/excalidraw/types";
import type { ThemePreference } from "./desktop";

// No "Open"/"Save to…" (canvases save themselves), and export to .excalidraw
// lives in the canvas list's context menu.
export const UI_OPTIONS: Partial<UIOptions> = {
  canvasActions: {
    loadScene: false,
    saveToActiveFile: false,
    export: false,
    saveAsImage: true,
    clearCanvas: true,
    changeViewBackgroundColor: true,
    toggleTheme: true,
  },
};

const svgProps = {
  viewBox: "0 0 24 24",
  fill: "none",
  stroke: "currentColor",
  strokeWidth: 1.5,
  strokeLinecap: "round" as const,
  strokeLinejoin: "round" as const,
};

export const homeIcon = (
  <svg {...svgProps}>
    <path d="M4 4h6v6h-6z" />
    <path d="M14 4h6v6h-6z" />
    <path d="M4 14h6v6h-6z" />
    <path d="M14 14h6v6h-6z" />
  </svg>
);

export const pencilIcon = (
  <svg {...svgProps}>
    <path d="M4 20h4l10.5 -10.5a2.828 2.828 0 1 0 -4 -4l-10.5 10.5v4" />
    <path d="M13.5 6.5l4 4" />
  </svg>
);

export const linkIcon = (
  <svg {...svgProps}>
    <path d="M9 15l6 -6" />
    <path d="M11 6l.463 -.536a5 5 0 0 1 7.071 7.072l-.534 .464" />
    <path d="M13 18l-.397 .534a5.068 5.068 0 0 1 -7.127 0a4.972 4.972 0 0 1 0 -7.071l.524 -.463" />
  </svg>
);

export const sparklesIcon = (
  <svg {...svgProps}>
    <path d="M16 18a2 2 0 0 1 2 2a2 2 0 0 1 2 -2a2 2 0 0 1 -2 -2a2 2 0 0 1 -2 2z" />
    <path d="M16 6a2 2 0 0 1 2 2a2 2 0 0 1 2 -2a2 2 0 0 1 -2 -2a2 2 0 0 1 -2 2z" />
    <path d="M9 18a6 6 0 0 1 6 -6a6 6 0 0 1 -6 -6a6 6 0 0 1 -6 6a6 6 0 0 1 6 6z" />
  </svg>
);

type ChipProps = {
  name: string | undefined;
  renaming: boolean;
  setRenaming: (renaming: boolean) => void;
  onRename: (name: string) => void;
  onGoHome: () => void;
  /** Extra buttons after the name (e.g. "Copy invite link"). */
  children?: ReactNode;
};

/** Top-right island: back to canvases + the canvas name (click to rename). */
export function CanvasChip({ name, renaming, setRenaming, onRename, onGoHome, children }: ChipProps) {
  return (
    <div className="canvas-chip">
      <button className="canvas-chip__home" title="All canvases (⌘O)" onClick={onGoHome}>
        {homeIcon}
      </button>
      {renaming ? (
        <input
          className="canvas-chip__input"
          autoFocus
          defaultValue={name}
          maxLength={120}
          spellCheck={false}
          onFocus={(e) => e.currentTarget.select()}
          onBlur={(e) => {
            setRenaming(false);
            const next = e.currentTarget.value.trim();
            if (next && next !== name) onRename(next);
          }}
          onKeyDown={(e) => {
            e.stopPropagation(); // keep Excalidraw shortcuts out of the field
            if (e.key === "Enter") e.currentTarget.blur();
            if (e.key === "Escape") {
              e.currentTarget.value = name ?? "";
              e.currentTarget.blur();
            }
          }}
        />
      ) : (
        <button className="canvas-chip__name" title="Rename canvas" onClick={() => setRenaming(true)}>
          {name}
        </button>
      )}
      {children}
    </div>
  );
}

type MenuProps = {
  onGoHome: () => void;
  onRename: () => void;
  themePreference: ThemePreference;
  onThemeChange: (theme: ThemePreference) => void;
  /** Items shown after "Rename canvas". */
  extraItems?: ReactNode;
};

export function EditorMainMenu({ onGoHome, onRename, themePreference, onThemeChange, extraItems }: MenuProps) {
  return (
    <MainMenu>
      <MainMenu.Item icon={homeIcon} onSelect={onGoHome} shortcut="Cmd+O">
        All canvases
      </MainMenu.Item>
      <MainMenu.Item icon={pencilIcon} onSelect={onRename}>
        Rename canvas
      </MainMenu.Item>
      {extraItems}
      <MainMenu.Separator />
      <MainMenu.DefaultItems.SaveAsImage />
      <MainMenu.DefaultItems.CommandPalette />
      <MainMenu.DefaultItems.SearchMenu />
      <MainMenu.DefaultItems.Help />
      <MainMenu.DefaultItems.ClearCanvas />
      <MainMenu.Separator />
      <MainMenu.DefaultItems.ToggleTheme allowSystemTheme theme={themePreference} onSelect={onThemeChange} />
      <MainMenu.DefaultItems.ChangeCanvasBackground />
    </MainMenu>
  );
}

/**
 * Runs on every editor change: keeps the library sidebar shut and follows
 * theme toggles made from inside Excalidraw.
 */
export function useEditorHousekeeping(
  apiRef: RefObject<ExcalidrawImperativeAPI | null>,
  effectiveTheme: "light" | "dark",
  onThemeChange: (theme: ThemePreference) => void,
) {
  const lastSeenTheme = useRef<string | null>(null);
  return useCallback(
    (appState: AppState) => {
      // No libraries in this app: close the sidebar whenever something tries
      // to open its library tab (command palette, API…). Search stays.
      const sidebar = appState.openSidebar;
      if (sidebar?.name === "default" && sidebar.tab !== "search") {
        queueMicrotask(() => apiRef.current?.updateScene({ appState: { openSidebar: null } }));
      }

      // Theme toggled from inside Excalidraw (e.g. Alt+Shift+D). Only react to
      // actual changes so we don't fight a theme prop that's mid-update.
      if (lastSeenTheme.current !== appState.theme) {
        const changed = lastSeenTheme.current !== null;
        lastSeenTheme.current = appState.theme;
        if (changed && appState.theme !== effectiveTheme) onThemeChange(appState.theme);
      }
    },
    [apiRef, effectiveTheme, onThemeChange],
  );
}

/** Same settle time Excalidraw uses after ⌘-scroll zooming. */
const PINCH_SETTLE_DELAY = 300;

/**
 * WebKit delivers trackpad pinches as Safari `gesture*` events, and
 * Excalidraw's handler for those redraws every visible shape from scratch at
 * each zoom step. Its ⌘-scroll zoom avoids that by stretching the shapes it
 * already drew until the zoom settles (`shouldCacheIgnoreZoom`), then redrawing
 * them sharp once. Do the same for pinches.
 */
export function usePinchZoomCache(apiRef: RefObject<ExcalidrawImperativeAPI | null>) {
  useEffect(() => {
    let settleTimer: number | undefined;
    const setIgnoreZoom = (shouldCacheIgnoreZoom: boolean) =>
      apiRef.current?.updateScene({ appState: { shouldCacheIgnoreZoom }, captureUpdate: CaptureUpdateAction.NEVER });
    const onPinch = () => {
      if (!apiRef.current?.getAppState().shouldCacheIgnoreZoom) setIgnoreZoom(true);
      window.clearTimeout(settleTimer);
      settleTimer = window.setTimeout(() => setIgnoreZoom(false), PINCH_SETTLE_DELAY);
    };
    document.addEventListener("gesturestart", onPinch);
    document.addEventListener("gesturechange", onPinch);
    return () => {
      window.clearTimeout(settleTimer);
      document.removeEventListener("gesturestart", onPinch);
      document.removeEventListener("gesturechange", onPinch);
    };
  }, [apiRef]);
}

/** Exposes the editor API for debugging from the Web Inspector. */
export function exposeApi(api: ExcalidrawImperativeAPI) {
  (window as { excalidrawAPI?: ExcalidrawImperativeAPI }).excalidrawAPI = api;
}
