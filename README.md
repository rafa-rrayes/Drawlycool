# Drawlycool

A hand-drawn whiteboard for macOS, built on the open-source [Excalidraw](https://github.com/excalidraw/excalidraw) editor (MIT).

- Opens to a **Welcome** screen listing your canvases. Each canvas is isolated and auto-saves.
- Canvases can be shared: host them from your Mac, or connect to someone else's server and draw together live.
- No library, no sign-up, no socials, no cloud. Local canvases work fully offline because fonts and assets are bundled.

## Install

Requires a Mac with Apple silicon and macOS 12 or later. To install or update, run:

```sh
curl -fsSL https://raw.githubusercontent.com/rafa-rrayes/Drawlycool/main/install.sh | bash
```

Or download `Drawlycool.dmg` from the [latest release](https://github.com/rafa-rrayes/Drawlycool/releases/latest) and drag the app to Applications. The app is not notarized. On first launch, macOS will say it can't verify the developer. Open it anyway from System Settings → Privacy & Security → *Open Anyway*. The install script skips this step.

## Usage

| Action | How |
| --- | --- |
| New canvas | Click **+ New canvas**, or press ⌘N |
| Back to canvases | Top-right grid button, main menu → *All canvases*, or press ⌘O |
| Rename | Click the canvas name (top-right), or right-click a card → *Rename…* |
| Duplicate / Export / Show in Finder / Delete | Right-click a card (or its `•••` button) |
| Import `.excalidraw` files | File → Import…, or drag the files onto the Welcome screen |
| Save | Automatic. ⌘S forces an immediate save |

Local data lives in `~/Library/Application Support/Drawlycool/canvases/`. On first launch, data from the app's earlier name (`…/Excalidraw/`) is moved there. Canvases you host are in `hosted/canvases/` in the same folder. Both use this layout:
- `index.json` holds canvas names and dates.
- Each `<id>.excalidraw` file is a standard Excalidraw scene, so it opens in excalidraw.com and other Excalidraw tools as well.

## Collaboration

Shared canvases live on a **server**. Any Mac running the app can act as one, or you can run the standalone `drawlycool-server` on a machine that is always on.

| Action | How |
| --- | --- |
| Host from your Mac | Welcome screen → **Host** (or File → Host on This Mac…), then turn on *Share canvases from this Mac* |
| Invite someone | **Copy invite link** in the Host panel, next to a server's name, or on an open canvas (link icon by its name) |
| Join | Click a `drawlycool://` invite link, or Welcome screen → **Connect…** and paste it |
| Create a shared canvas | **+ New canvas** under the server's section on the Welcome screen |
| Share an existing canvas | Right-click a local card → *Move to* → server |
| Your name on other people's screens | Click your avatar (top-right of the Welcome screen) |

In a shared canvas you see everyone's cursors, selections and names. Click someone's avatar to follow their viewport. Edits merge per element, so if two people move the *same* shape at once, one change wins. If the connection drops, the canvas turns read-only with a *Reconnecting…* banner until it comes back. There is no offline editing of shared canvases.

**Reachability.** The Host panel lists this Mac's addresses: the `.local` name and LAN IP for people on the same network, the Tailscale IP for people on your tailnet, and a public IP if you have one. Pick the one your guests can reach. They are embedded in the invite link. A hosting Mac must be awake with Drawlycool open. The first time you host, macOS may ask to allow incoming connections (firewall) and Local Network access.

**Security.** An invite link contains the server's access token. Anyone holding it can open, edit and delete every canvas on that server. Share it like a password. *Reset access* in the Host panel issues a new token and invalidates old links. Traffic is plain HTTP/WebSocket unless the server sits behind a TLS proxy, so stay on a trusted network or Tailscale/VPN. Tokens for servers you've joined are kept in plain text in `servers.json` in the app's data folder.

### Deploy a server for your team

On any Linux server with a domain pointing at it (ports 80 and 443 open), run:

```sh
curl -fsSL https://raw.githubusercontent.com/rafa-rrayes/Drawlycool/main/deploy.sh | sudo bash -s -- --domain draw.example.com --name "Acme"
```

The script:
- installs Docker if it's missing;
- starts the server and [Caddy](https://caddyserver.com), which gets an HTTPS certificate automatically;
- prints the invite link and saves it in `/opt/drawlycool/invite.txt`.

To run plain HTTP on port 4180 instead, for a LAN or VPN, leave out `--domain`. Run the same command again to update. Canvases and the token are kept in a Docker volume. Other options: `--url`, `--port`, `--dir`, and `--reset-token`, which issues a new token and stops old invite links from working.

Or run the image directly:

```sh
docker run -d --name drawlycool --restart unless-stopped -p 4180:4180 -v drawlycool:/data \
  -e DRAWLYCOOL_NAME="Acme" -e DRAWLYCOOL_PUBLIC_URL=http://10.0.0.5:4180 \
  ghcr.io/rafa-rrayes/drawlycool-server
docker logs drawlycool   # shows the invite link
```

The image is built for amd64 and arm64 from `server/Dockerfile` by `.github/workflows/server-image.yml`. It is about 36 MB, runs as a non-root user, and saves on `docker stop`.

### Standalone server

Without Docker:

```sh
cd server
cargo run --release -- --data ./drawlycool-data --port 4180 --name "Studio"
# prints: Invite link: drawlycool://connect?server=…&token=…
```

Options: `--data <dir>`, `--port <port>` (default 4180), `--host <ip>` (default 0.0.0.0), `--name <name>`, and `--public-url <url>` (the URL guests use, e.g. `https://draw.example.com` behind a TLS reverse proxy that forwards WebSockets). `DRAWLYCOOL_NAME` and `DRAWLYCOOL_PUBLIC_URL` work in place of those two flags. The token comes from `DRAWLYCOOL_TOKEN`, or is generated once into `<data>/token`. Ctrl-C or SIGTERM saves and stops.

Server data uses the same format as the app: `index.json` plus one `<id>.excalidraw` per canvas. Scenes are saved about a second after the last edit. Deleted elements are pruned after 24 hours.

## Development

Requires Node and Rust 1.90 or newer.

```sh
npm install        # also copies Excalidraw's fonts into public/fonts
npm run dev        # Vite + Tauri with hot reload
npm run dist       # builds src-tauri/target/release/bundle/{macos/Drawlycool.app, dmg/*.dmg}
```

Layout:
- `server/`: the sync server, both a library and the `drawlycool-server` binary. It contains:
  - `storage.rs`: canvas files. Writes are atomic and serialized under a mutex. The app uses it for local canvases too.
  - `scene.rs`: element merging by version/nonce, the same rule Excalidraw uses.
  - `lib.rs`: HTTP API, WebSocket rooms, presence, follow, and debounced saves.
  - `tests/sync.rs`: run with `cargo test` in `server/`.
- `src-tauri/src/lib.rs`: window, native menu, commands, and the quit-time save flush.
- `src-tauri/src/network.rs`: saved servers, invite links, and live sessions. All networking runs in Rust; the webview only sees a channel.
- `src-tauri/src/host.rs`: runs the server inside the app when hosting.
- `src-tauri/src/e2e.rs`: a test hook that runs JS in the webview. It only exists in debug builds.
- `src/desktop.ts`: the frontend's bridge to Rust. Scenes are sent as raw bytes, not JSON.
- `src/platform.ts`: routes external links, "save to disk" downloads and the window title to native equivalents.
- `src/Home.tsx`: the Welcome screen. Card outlines are drawn with rough.js.
- `src/CanvasEditor.tsx`: the Excalidraw editor with a trimmed menu, auto-save and library blocking.
- `src/RemoteCanvasEditor.tsx`: the same editor connected to a server, with reconcile, cursors, follow and reconnect.
- `src/Panels.tsx`: the Connect, Host and profile dialogs.

The app is ad-hoc signed, not notarized (see *Install*). To publish a release, run `npm run dist` and attach `src-tauri/target/release/bundle/dmg/*.dmg` as `Drawlycool.dmg`. The install script always downloads `releases/latest/download/Drawlycool.dmg`.
