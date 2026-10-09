// Excalidraw colours collaborators by hashing their id (it ignores any colour
// we pass in), so the Welcome screen uses the same formula to match.

function hashToInteger(id: string) {
  let hash = 0;
  for (let i = 0; i < id.length; i++) {
    hash = (hash << 5) - hash + id.charCodeAt(i);
  }
  return hash;
}

/** Same as Excalidraw's `getClientColor`. */
export function clientColor(id: string) {
  const hue = (Math.abs(hashToInteger(id)) % 37) * 10;
  return `hsl(${hue}, 100%, 83%)`;
}

export function initials(name: string) {
  const parts = name.trim().split(/\s+/).filter(Boolean);
  const letters = parts.length > 1 ? parts[0][0] + parts[parts.length - 1][0] : (parts[0] ?? "?").slice(0, 2);
  return letters.toUpperCase();
}
