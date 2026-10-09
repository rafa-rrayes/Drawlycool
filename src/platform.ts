// Glue between browser behaviours Excalidraw relies on and the native app.
import { getCurrentWindow } from "@tauri-apps/api/window";
import { desktop } from "./desktop";

const appWindow = getCurrentWindow();

const isExternal = (url: string) => /^(https?:|mailto:)/i.test(url);

// External links (canvas element links, Help dialog…) open in the default browser.
window.open = ((url?: string | URL) => {
  const href = url?.toString() ?? "";
  if (isExternal(href)) desktop.openUrl(href);
  return null;
}) as typeof window.open;

/**
 * WebKit can't save `<a download>` blobs to disk on its own, and Excalidraw
 * uses exactly that for "Export image → Save to disk". Route it through a
 * native save dialog instead.
 */
async function saveAnchorDownload(anchor: HTMLAnchorElement) {
  const blob = await (await fetch(anchor.href)).blob();
  await desktop.saveDownload(anchor.download || "download", new Uint8Array(await blob.arrayBuffer()));
}

function handleAnchor(anchor: HTMLAnchorElement): boolean {
  if (anchor.hasAttribute("download") && /^(blob|data):/.test(anchor.href)) {
    saveAnchorDownload(anchor).catch((err) => console.error("Save failed", err));
    return true;
  }
  if (isExternal(anchor.href)) {
    desktop.openUrl(anchor.href);
    return true;
  }
  return false;
}

// Detached anchors (`a.click()` without inserting into the DOM) never reach a
// document listener, so patch click() itself.
const nativeClick = HTMLAnchorElement.prototype.click;
HTMLAnchorElement.prototype.click = function (this: HTMLAnchorElement) {
  if (!handleAnchor(this)) nativeClick.call(this);
};

document.addEventListener(
  "click",
  (event) => {
    const anchor = (event.target as Element | null)?.closest?.("a");
    if (anchor && handleAnchor(anchor)) event.preventDefault();
  },
  true,
);

// No "Reload / Inspect Element" menu. Text fields keep their native one.
document.addEventListener("contextmenu", (event) => {
  const target = event.target as HTMLElement;
  if (!target.closest("input, textarea, [contenteditable='true']")) event.preventDefault();
});

// Mirror document.title into the native window title.
new MutationObserver(() => appWindow.setTitle(document.title)).observe(
  document.querySelector("title")!,
  { childList: true, characterData: true, subtree: true },
);

/**
 * The window starts hidden; reveal it once React has rendered. (Not via
 * requestAnimationFrame: hidden pages never get animation frames.)
 */
export function showWindowWhenReady() {
  setTimeout(() => appWindow.show(), 30);
}
