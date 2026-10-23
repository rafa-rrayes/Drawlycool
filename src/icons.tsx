// Hand-drawn icons: plain 24×24 line icons traced through rough.js, so they
// wobble like everything else drawn in the app.
import rough from "roughjs";

const generator = rough.generator();

const ICONS = {
  plus: "M12 5v14 M5 12h14",
  laptop: "M5 16V7a1 1 0 0 1 1-1h12a1 1 0 0 1 1 1v9 M2.5 19h19",
  friends:
    "M9 7m-4 0a4 4 0 1 0 8 0a4 4 0 1 0 -8 0 M3 21v-2a4 4 0 0 1 4-4h4a4 4 0 0 1 4 4v2 " +
    "M16 3.13a4 4 0 0 1 0 7.75 M21 21v-2a4 4 0 0 0-3-3.85",
  cloud:
    "M6.66 18C4.09 18 2 15.99 2 13.52c0-2.48 2.09-4.48 4.66-4.48c.39-1.76 1.79-3.2 3.67-3.77c1.88-.57 3.96-.19 5.45 1 " +
    "c1.49 1.19 2.16 3.01 1.77 4.77h.99c1.91 0 3.46 1.56 3.46 3.49c0 1.93-1.55 3.49-3.46 3.49z",
  link:
    "M9 15l6-6 M11 6l.46-.54a5 5 0 0 1 7.07 7.07l-.53.47 " +
    "M13 18l-.4.53a5.07 5.07 0 0 1-7.13 0a4.97 4.97 0 0 1 0-7.07l.53-.46",
  sparkles:
    "M16 18a2 2 0 0 1 2 2a2 2 0 0 1 2-2a2 2 0 0 1-2-2a2 2 0 0 1-2 2z M16 6a2 2 0 0 1 2 2a2 2 0 0 1 2-2a2 2 0 0 1-2-2a2 2 0 0 1-2 2z " +
    "M9 18a6 6 0 0 1 6-6a6 6 0 0 1-6-6a6 6 0 0 1-6 6a6 6 0 0 1 6 6z",
  gear:
    "M10.33 4.32c.43-1.76 2.92-1.76 3.35 0a1.72 1.72 0 0 0 2.57 1.07c1.54-.94 3.31.83 2.37 2.37a1.72 1.72 0 0 0 1.07 2.57 " +
    "c1.76.43 1.76 2.92 0 3.35a1.72 1.72 0 0 0-1.07 2.57c.94 1.54-.83 3.31-2.37 2.37a1.72 1.72 0 0 0-2.57 1.07 " +
    "c-.43 1.76-2.92 1.76-3.35 0a1.72 1.72 0 0 0-2.57-1.07c-1.54.94-3.31-.83-2.37-2.37a1.72 1.72 0 0 0-1.07-2.57 " +
    "c-1.76-.43-1.76-2.92 0-3.35a1.72 1.72 0 0 0 1.07-2.57c-.94-1.54.83-3.31 2.37-2.37c1 .61 2.3.07 2.57-1.07z " +
    "M9 12a3 3 0 1 0 6 0a3 3 0 0 0-6 0",
  trash:
    "M4 7h16 M10 11v6 M14 11v6 M5 7l1 12a2 2 0 0 0 2 2h8a2 2 0 0 0 2-2l1-12 M9 7V4a1 1 0 0 1 1-1h4a1 1 0 0 1 1 1v3",
  join: "M9 8V6a2 2 0 0 1 2-2h7a2 2 0 0 1 2 2v12a2 2 0 0 1-2 2h-7a2 2 0 0 1-2-2v-2 M3 12h13 M13 9l3 3-3 3",
  moon: "M12 3h.39a7.5 7.5 0 0 0 7.92 12.45a9 9 0 1 1-8.31-12.45z",
  check: "M5 12l5 5L20 7",
  retry: "M20 11a8.1 8.1 0 0 0-15.5-2 M4 5v4h4 M4 13a8.1 8.1 0 0 0 15.5 2 M20 19v-4h-4",
  close: "M18 6L6 18 M6 6l12 12",
  more: "M4 12a1 1 0 1 0 2 0a1 1 0 1 0-2 0 M11 12a1 1 0 1 0 2 0a1 1 0 1 0-2 0 M18 12a1 1 0 1 0 2 0a1 1 0 1 0-2 0",
  folder: "M5 4h4l3 3h7a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V6a2 2 0 0 1 2-2",
  folderPlus: "M12 19H5a2 2 0 0 1-2-2V6a2 2 0 0 1 2-2h4l3 3h7a2 2 0 0 1 2 2v3.5 M16 19h6 M19 16v6",
} as const;

export type IconName = keyof typeof ICONS;

// Icons never change shape, so trace each one once.
const traced = new Map<IconName, string[]>();

function trace(name: IconName) {
  let paths = traced.get(name);
  if (!paths) {
    const drawable = generator.path(ICONS[name], {
      seed: 3,
      roughness: 0.55,
      bowing: 0.6,
      strokeWidth: 1.6,
      preserveVertices: true,
    });
    paths = generator.toPaths(drawable).map((p) => p.d);
    traced.set(name, paths);
  }
  return paths;
}

export function DrawnIcon({ name, size = 22, className }: { name: IconName; size?: number; className?: string }) {
  return (
    <svg
      className={`drawn-icon ${className ?? ""}`}
      viewBox="0 0 24 24"
      width={size}
      height={size}
      aria-hidden
      fill="none"
      stroke="currentColor"
      strokeWidth={1.6}
      strokeLinecap="round"
      strokeLinejoin="round"
    >
      {trace(name).map((d, i) => (
        <path key={i} d={d} />
      ))}
    </svg>
  );
}
