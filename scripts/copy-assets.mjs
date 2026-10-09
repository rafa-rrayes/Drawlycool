// Copies Excalidraw's font files into public/ so the app works fully offline.
import { cpSync, rmSync } from "node:fs";
import { fileURLToPath } from "node:url";
import path from "node:path";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const src = path.join(root, "node_modules/@excalidraw/excalidraw/dist/prod/fonts");
const dest = path.join(root, "public/fonts");

rmSync(dest, { recursive: true, force: true });
cpSync(src, dest, { recursive: true });
console.log("Copied Excalidraw fonts to public/fonts");
