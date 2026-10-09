import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { Excalidraw, hashElementsVersion, serializeAsJSON } from "@excalidraw/excalidraw";
import type {
  AppState,
  BinaryFiles,
  ExcalidrawImperativeAPI,
  ExcalidrawInitialDataState,
} from "@excalidraw/excalidraw/types";
import type { OrderedExcalidrawElement } from "@excalidraw/excalidraw/element/types";
import { desktop, type CanvasMeta, type ThemePreference } from "./desktop";
import { CanvasChip, EditorMainMenu, UI_OPTIONS, exposeApi, useEditorHousekeeping } from "./editorChrome";

const SCENE_SAVE_DELAY = 400;
const VIEWPORT_SAVE_DELAY = 800;

type Latest = {
  elements: readonly OrderedExcalidrawElement[];
  appState: AppState;
  files: BinaryFiles;
};

type Props = {
  id: string;
  themePreference: ThemePreference;
  effectiveTheme: "light" | "dark";
  onThemeChange: (theme: ThemePreference) => void;
  onGoHome: () => void;
  /** Lets the app flush pending saves before navigating or quitting. */
  registerFlush: (flush: (() => Promise<void>) | null) => void;
};

export function CanvasEditor({ id, themePreference, effectiveTheme, onThemeChange, onGoHome, registerFlush }: Props) {
  const [meta, setMeta] = useState<CanvasMeta | null>(null);
  const [initialData, setInitialData] = useState<ExcalidrawInitialDataState | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [renaming, setRenaming] = useState(false);
  const apiRef = useRef<ExcalidrawImperativeAPI | null>(null);

  // ------------------------------------------------------------ loading

  useEffect(() => {
    let cancelled = false;
    desktop
      .load(id)
      .then(({ meta, scene, viewport }) => {
        if (cancelled) return;
        const data = JSON.parse(scene);
        setMeta(meta);
        setInitialData({
          elements: data.elements ?? [],
          files: data.files ?? {},
          appState: {
            ...data.appState,
            ...(viewport && {
              scrollX: viewport.scrollX,
              scrollY: viewport.scrollY,
              zoom: { value: viewport.zoom },
            }),
          },
          scrollToContent: !viewport,
        });
      })
      .catch((err) => !cancelled && setError(String(err?.message ?? err)));
    return () => {
      cancelled = true;
    };
  }, [id]);

  useEffect(() => {
    document.title = meta ? `${meta.name} — Drawlycool` : "Drawlycool";
    return () => {
      document.title = "Drawlycool";
    };
  }, [meta]);

  // ------------------------------------------------------------ saving

  const latest = useRef<Latest | null>(null);
  const savedSceneSig = useRef<string | null>(null);
  const savedViewportSig = useRef<string | null>(null);
  const sceneTimer = useRef<number>(undefined);
  const viewportTimer = useRef<number>(undefined);
  const housekeeping = useEditorHousekeeping(apiRef, effectiveTheme, onThemeChange);

  const sceneSignature = ({ elements, appState, files }: Latest) =>
    [
      hashElementsVersion(elements),
      appState.viewBackgroundColor,
      appState.gridModeEnabled,
      appState.gridSize,
      appState.gridStep,
      Object.keys(files).sort().join(","),
    ].join("|");

  const viewportSignature = ({ appState }: Latest) =>
    `${appState.scrollX}|${appState.scrollY}|${appState.zoom.value}`;

  const saveScene = useCallback(async () => {
    window.clearTimeout(sceneTimer.current);
    const state = latest.current;
    if (!state) return;
    const sig = sceneSignature(state);
    if (sig === savedSceneSig.current) return;
    savedSceneSig.current = sig;
    const json = serializeAsJSON(state.elements, state.appState, state.files, "local");
    try {
      await desktop.saveScene(id, json);
    } catch (err) {
      savedSceneSig.current = "unsaved"; // retry on the next change or flush
      console.error("Failed to save canvas", err);
    }
  }, [id]);

  const saveViewport = useCallback(async () => {
    window.clearTimeout(viewportTimer.current);
    const state = latest.current;
    if (!state) return;
    const sig = viewportSignature(state);
    if (sig === savedViewportSig.current) return;
    savedViewportSig.current = sig;
    const { scrollX, scrollY, zoom } = state.appState;
    await desktop.saveViewport(id, { scrollX, scrollY, zoom: zoom.value });
  }, [id]);

  const flush = useCallback(async () => {
    await Promise.all([saveScene(), saveViewport()]);
  }, [saveScene, saveViewport]);

  useEffect(() => {
    registerFlush(flush);
    return () => {
      registerFlush(null);
      flush();
    };
  }, [flush, registerFlush]);

  const onChange = useCallback(
    (elements: readonly OrderedExcalidrawElement[], appState: AppState, files: BinaryFiles) => {
      const state = { elements, appState, files };
      latest.current = state;

      // First change is the freshly loaded scene: record it as the baseline so
      // merely opening a canvas doesn't touch its "edited" time.
      if (savedSceneSig.current === null) {
        savedSceneSig.current = sceneSignature(state);
        savedViewportSig.current = viewportSignature(state);
        return;
      }

      if (sceneSignature(state) !== savedSceneSig.current) {
        window.clearTimeout(sceneTimer.current);
        sceneTimer.current = window.setTimeout(saveScene, SCENE_SAVE_DELAY);
      }
      if (viewportSignature(state) !== savedViewportSig.current) {
        window.clearTimeout(viewportTimer.current);
        viewportTimer.current = window.setTimeout(saveViewport, VIEWPORT_SAVE_DELAY);
      }

      housekeeping(appState);
    },
    [saveScene, saveViewport, housekeeping],
  );

  // ------------------------------------------------------------ rename

  const commitRename = useCallback(
    async (name: string) => {
      if (meta) setMeta(await desktop.rename(meta.id, name));
    },
    [meta],
  );

  const renderTopRightUI = useCallback(
    () => (
      <CanvasChip
        name={meta?.name}
        renaming={renaming}
        setRenaming={setRenaming}
        onRename={commitRename}
        onGoHome={onGoHome}
      />
    ),
    [meta, renaming, commitRename, onGoHome],
  );

  const mainMenu = useMemo(
    () => (
      <EditorMainMenu
        onGoHome={onGoHome}
        onRename={() => setRenaming(true)}
        themePreference={themePreference}
        onThemeChange={onThemeChange}
      />
    ),
    [onGoHome, themePreference, onThemeChange],
  );

  if (error) {
    return (
      <div className="canvas-error">
        <p>Couldn’t open this canvas.</p>
        <pre>{error}</pre>
        <button onClick={onGoHome}>Back to canvases</button>
      </div>
    );
  }

  if (!initialData) return <div className="canvas-loading" />;

  return (
    <div className="canvas-editor">
      <Excalidraw
        excalidrawAPI={(api) => {
          apiRef.current = api;
          exposeApi(api);
        }}
        initialData={initialData}
        onChange={onChange}
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
    </div>
  );
}
