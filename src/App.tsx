import { ui } from "./ui";
import { useCallback, useEffect, useRef, useState } from "react";
import { CanvasEditor } from "./CanvasEditor";
import { Home } from "./Home";
import type { Panel } from "./Panels";
import { RemoteCanvasEditor } from "./RemoteCanvasEditor";
import { desktop, type Profile, type ServerInfo, type ThemePreference } from "./desktop";

type Route =
  | { view: "home" }
  | { view: "canvas"; id: string }
  | { view: "remote"; serverId: string; serverName: string; canvasId: string };

const darkQuery = window.matchMedia("(prefers-color-scheme: dark)");

function useSystemDark() {
  const [dark, setDark] = useState(darkQuery.matches);
  useEffect(() => {
    const onChange = () => setDark(darkQuery.matches);
    darkQuery.addEventListener("change", onChange);
    return () => darkQuery.removeEventListener("change", onChange);
  }, []);
  return dark;
}

export function App() {
  const [route, setRoute] = useState<Route>({ view: "home" });
  const [themePreference, setThemePreference] = useState<ThemePreference>("system");
  const [profile, setProfile] = useState<Profile | null>(null);
  const [panel, setPanel] = useState<Panel | null>(null);
  const [pendingNewCanvas, setPendingNewCanvas] = useState(false);
  const [refreshKey, setRefreshKey] = useState(0);
  const flushRef = useRef<(() => Promise<void>) | null>(null);

  const systemDark = useSystemDark();
  const effectiveTheme =
    themePreference === "system" ? (systemDark ? "dark" : "light") : themePreference;

  useEffect(() => {
    desktop.getSettings().then(async (s) => {
      if (s.theme) setThemePreference(s.theme);
      if (s.profile?.name && s.profile.id) return setProfile(s.profile);
      // First run: introduce yourself with the macOS account name.
      const fresh = {
        name: s.profile?.name || (await desktop.userFullName()),
        id: crypto.randomUUID(),
      };
      setProfile(fresh);
      desktop.setSettings({ profile: fresh });
    });
  }, []);

  useEffect(() => {
    document.documentElement.dataset.theme = effectiveTheme;
  }, [effectiveTheme]);

  const changeTheme = useCallback((theme: ThemePreference) => {
    setThemePreference(theme);
    desktop.setSettings({ theme });
  }, []);

  const changeProfile = useCallback((next: Profile) => {
    setProfile(next);
    desktop.setSettings({ profile: next });
  }, []);

  const registerFlush = useCallback((flush: (() => Promise<void>) | null) => {
    flushRef.current = flush;
  }, []);

  const goHome = useCallback(async () => {
    await flushRef.current?.();
    setRoute({ view: "home" });
  }, []);

  const openHostSettings = useCallback(async () => {
    await goHome();
    setPanel({ kind: "host" });
  }, [goHome]);

  const openCanvas = useCallback(async (id: string) => {
    await flushRef.current?.();
    setRoute({ view: "canvas", id });
  }, []);

  const openRemote = useCallback(async (server: ServerInfo, canvasId: string) => {
    await flushRef.current?.();
    setPanel(null);
    setRoute({ view: "remote", serverId: server.id, serverName: server.name, canvasId });
  }, []);

  useEffect(
    () =>
      desktop.onMenu(async ({ action }) => {
        switch (action) {
          case "go-home":
            return goHome();
          case "new-canvas":
            await goHome();
            return setPendingNewCanvas(true);
          case "import": {
            const { imported, errors } = await desktop.importDialog();
            if (errors.length) ui.alert("Some files couldn’t be imported", errors.join("\n"));
            if (!imported.length) return;
            await goHome();
            return setRefreshKey((n) => n + 1);
          }
          case "save":
            return flushRef.current?.();
          case "connect":
            await goHome();
            return setPanel({ kind: "connect" });
          case "host":
            await goHome();
            return setPanel({ kind: "host" });
        }
      }),
    [goHome],
  );

  // drawlycool://connect?… links, whether they launched the app or arrived later.
  useEffect(() => {
    const takeLinks = async () => {
      const links = await desktop.servers.takePendingLinks();
      const link = links[links.length - 1];
      if (!link) return;
      await goHome();
      setPanel({ kind: "connect", link, auto: true });
    };
    takeLinks();
    return desktop.servers.onLinkOpened(takeLinks);
  }, [goHome]);

  useEffect(() => desktop.onFlushRequest(() => flushRef.current?.()), []);

  if (!profile) return <div className="canvas-loading" />;

  if (route.view === "canvas") {
    return (
      <CanvasEditor
        key={route.id}
        id={route.id}
        themePreference={themePreference}
        effectiveTheme={effectiveTheme}
        onThemeChange={changeTheme}
        onGoHome={goHome}
        registerFlush={registerFlush}
      />
    );
  }

  if (route.view === "remote") {
    return (
      <RemoteCanvasEditor
        key={`${route.serverId}:${route.canvasId}`}
        serverId={route.serverId}
        serverName={route.serverName}
        canvasId={route.canvasId}
        profile={profile}
        themePreference={themePreference}
        effectiveTheme={effectiveTheme}
        onThemeChange={changeTheme}
        onGoHome={goHome}
        onHostSettings={openHostSettings}
        registerFlush={registerFlush}
      />
    );
  }

  return (
    <Home
      onOpen={openCanvas}
      onOpenRemote={openRemote}
      startNewCanvas={pendingNewCanvas}
      onStartNewCanvasHandled={() => setPendingNewCanvas(false)}
      refreshKey={refreshKey}
      profile={profile}
      onProfileChange={changeProfile}
      panel={panel}
      setPanel={setPanel}
    />
  );
}
