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
| Duplicate / Export / Show in Finder / Delete | Right-click a card (or its `•••` button). Or drag a card to the trash that appears at the bottom |
| Arrange | Drag cards around. The order is remembered |
| Folders | Folder icon next to a space's name → name it. Drag canvases onto a folder card to file them (or right-click → *Move to*). Click a folder to look inside; click the space's name above it to go back, or drop a canvas there to take it out. Deleting a folder keeps its canvases |
| Import `.excalidraw` files | File → Import…, or drag the files onto the Welcome screen |
| Save | Automatic. ⌘S forces an immediate save |

Local data lives in `~/Library/Application Support/Drawlycool/canvases/`. On first launch, data from the app's earlier name (`…/Excalidraw/`) is moved there. Canvases in *With friends* (shared from this Mac) are in `hosted/canvases/` in the same folder. Both use this layout:
- `index.json` holds canvas names and dates, and the folder each one is in.
- `folders.json` holds the folders.
- Each `<id>.excalidraw` file is a standard Excalidraw scene, so it opens in excalidraw.com and other Excalidraw tools as well.

## Collaboration

Shared canvases live on a **server**. Any Mac running the app can act as one, or you can run the standalone `drawlycool-server` on a machine that is always on.

| Action | How |
| --- | --- |
| Share from your Mac | Drag a canvas into **With friends** on the Welcome screen. Sharing turns on by itself. Settings: gear icon there, or File → Host on This Mac… |
| Invite someone | Link icon next to a space's name, or on an open canvas (link icon by its name) |
| Join | Click a `drawlycool://` invite link, paste one anywhere on the Welcome screen (⌘V), or **Join** (top-right). Servers you left are one click away there |
| Create a shared canvas | **+ New canvas** in that space on the Welcome screen |
| Move a canvas between spaces | Drag it into another space, or onto that space in the dock that appears while dragging. Also: right-click → *Move to*. It moves in either direction, so you can always bring it back to *Just me* |
| Your name on other people's screens | Click your avatar (top-right of the Welcome screen) |
| Let an AI assistant draw with you | Sparkles icon next to a space's name or a shared canvas's name. It shows the setup for Claude Code, Claude Desktop & claude.ai, and other MCP apps |

In a shared canvas you see everyone's cursors, selections and names. Click someone's avatar to follow their viewport. Edits merge per element, so if two people move the *same* shape at once, one change wins. If the connection drops, the canvas turns read-only with a *Reconnecting…* banner until it comes back. There is no offline editing of shared canvases.

While sharing is off, *With friends* still shows its canvases, dimmed. Clicking one turns sharing back on and opens it.

**Reachability.** Under *Who can join* in the sharing settings:

- **Anywhere** (the default). Drawlycool runs a free [Cloudflare quick tunnel](https://developers.cloudflare.com/cloudflare-one/networks/connectors/cloudflare-tunnel/do-more-with-tunnels/trycloudflare/), which gives this Mac a public `https://….trycloudflare.com` address without an account or router setup. It needs `cloudflared`, which isn't bundled. If it's missing, the panel shows a one-line install command (`brew install cloudflared`, or a download into `~/.local/bin` if you don't have Homebrew) and picks it up as soon as it's installed. The address changes each time Drawlycool restarts, so send a fresh link then.
- **Same Wi-Fi or VPN.** Pick one of this Mac's addresses: the `.local` name or LAN IP for people on the same network, the Tailscale IP for people on your tailnet, or a public IP if you have one.

Invite links look like `drawlycool://tidy-otter.trycloudflare.com/<token>`: the host plus the access token, and a canvas id if the link is for one canvas. A port in the link means plain `http://`, no port means `https://`. Older `drawlycool://connect?server=…&token=…` links still work. A hosting Mac must be awake with Drawlycool open. The first time you host, macOS may ask to allow incoming connections (firewall) and Local Network access.

**Security.** An invite link contains the server's access token. Anyone holding it can open, edit and delete every canvas on that server. Share it like a password. *Reset access* in the sharing settings issues a new token and invalidates old links. Traffic is plain HTTP/WebSocket unless it goes through the Cloudflare tunnel or the server sits behind a TLS proxy, so on the *Same Wi-Fi or VPN* option stay on a trusted network or Tailscale/VPN. The tunnel encrypts traffic between guests and Cloudflare, but Cloudflare can see it. Tokens for servers you've joined are kept in plain text in `servers.json` in the app's data folder.

### Draw with an AI assistant

Every server, including a Mac that hosts, has an [MCP](https://modelcontextprotocol.io) endpoint at `/mcp`. Claude and other AI assistants can use it to join a shared canvas as a participant. People see the assistant's name and cursor, and its drawing appears piece by piece. You can follow its view, and it can point things out with the laser. It also sees your cursor and selection, so "make this blue" works.

To set it up, click the sparkles icon next to a shared canvas's name, or choose *Connect AI Assistant…* in a server's `•••` menu, and copy the setup for your app:

- **Claude Code:** run the `claude mcp add …` command it shows, then ask Claude to draw something.
- **Claude Desktop or claude.ai:** Settings → Connectors → *Add custom connector*, then paste the URL. Cloud apps connect from the internet, so this needs a public `https://` address, such as *Anyone, over the internet* hosting or a deployed server. Without one, the panel gives a local bridge config for Claude Desktop that uses `npx mcp-remote`.
- **Other MCP apps** (Cursor, Windsurf, Codex…): use the JSON config. The endpoint is `<server>/mcp`, with `Authorization: Bearer <token>` or `?token=<token>`.
- **Optional query parameters:** `canvas=<id>` makes the assistant start in that canvas, and `name=<name>` sets the name shown next to its cursor.

The tools are built so a model can make tidy diagrams without doing coordinate math:

| Tool | What it does |
| --- | --- |
| `list_canvases`, `open_canvas` | Find, create and join canvases |
| `describe_canvas` | A compact inventory: ids, positions, labels, connections, who's here and what they've selected, and free space |
| `view_canvas` | A PNG of the canvas with a coordinate grid, so the model can check its work |
| `draw` | Shapes, text, arrows, lines, frames, notes and zones. Shapes size themselves to their labels. Elements can be placed relative to others (`below`, `right_of`…). Arrows bind to shapes, with straight, elbow or curved routes. Named palette colours. Reusing an id updates that element |
| `draw_diagram` | Automatic layouts: flowchart / layered graph, tree, mind map, radial, grid and sequence diagram, with clusters, titles and consistent styling |
| `arrange` | Move, align, distribute, row/column/grid, z-order, group, lock, frame |
| `delete_elements`, `point_at`, `update_canvas` | Clean up, point with the laser, rename or recolour the canvas |

Every change comes back with a layout check that reports overlaps, labels that don't fit, arrows crossing shapes and low-contrast text, so the model can fix them. The assistant leaves the canvas after 15 minutes without a tool call. Anyone holding the token can do everything the assistant can, so treat MCP settings like an invite link.

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
  - `mcp/`: the MCP endpoint. `mod.rs` handles the transport and sessions, `agent.rs` the assistant's presence (cursor, animated drawing, laser), `tools.rs` the tool schemas, and `draw.rs`, `diagram.rs` and `layout.rs` build and lay out elements. `build.rs` follows Excalidraw's own rules for labels, bindings and elbow arrows, `describe.rs` writes the scene text and the layout check, and `render.rs` makes the PNG previews. Text is measured with the editor's fonts (`assets/fonts`), so labels fit.
  - `tests/sync.rs`, `tests/mcp.rs`: run with `cargo test` in `server/`.
- `src-tauri/src/lib.rs`: window, native menu, commands, and the quit-time save flush.
- `src-tauri/src/network.rs`: saved servers, invite links, and live sessions. All networking runs in Rust; the webview only sees a channel.
- `src-tauri/src/host.rs`: runs the server inside the app when hosting.
- `src-tauri/src/tunnel.rs`: finds `cloudflared` and runs the Cloudflare quick tunnel for *Anywhere*.
- `src-tauri/src/e2e.rs`: a test hook that runs JS in the webview. It only exists in debug builds.
- `src/desktop.ts`: the frontend's bridge to Rust. Scenes are sent as raw bytes, not JSON.
- `src/platform.ts`: routes external links, "save to disk" downloads and the window title to native equivalents.
- `src/Home.tsx`: the Welcome screen: spaces (this Mac, shared from it, friends' servers) and their cards. Outlines are drawn with rough.js.
- `src/cardDrag.ts`: dragging cards to rearrange them, move them between spaces or delete them.
- `src/icons.tsx`: line icons traced through rough.js.
- `src/CanvasEditor.tsx`: the Excalidraw editor with a trimmed menu, auto-save and library blocking.
- `src/RemoteCanvasEditor.tsx`: the same editor connected to a server, with reconcile, cursors, follow and reconnect.
- `src/Panels.tsx`: the Join, sharing settings, AI assistant and profile dialogs.

The app is ad-hoc signed, not notarized (see *Install*). To publish a release, run `npm run dist` and attach `src-tauri/target/release/bundle/dmg/*.dmg` as `Drawlycool.dmg`. The install script always downloads `releases/latest/download/Drawlycool.dmg`.
