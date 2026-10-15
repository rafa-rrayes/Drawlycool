# Fonts

Excalidraw's bundled fonts (from `@excalidraw/excalidraw`'s `dist/prod/fonts`),
converted from `.woff2` to TrueType with hinting stripped. The MCP server uses
them to measure text exactly like the editor and to draw canvas previews. Each
family is split into unicode-range subsets, like the originals.

| Files | Font | License |
| --- | --- | --- |
| `Excalifont-*.ttf` | Excalifont | SIL Open Font License 1.1 |
| `Nunito-*.ttf` | Nunito | SIL Open Font License 1.1 |
| `ComicShanns-*.ttf` | Comic Shanns | MIT |
| `Lilita-*.ttf` | Lilita One | SIL Open Font License 1.1 |
| `Liberation-*.ttf` | Liberation Sans | SIL Open Font License 1.1 |
| `NotoSymbols-0.ttf`, `NotoSymbols2-0.ttf`, `NotoEmoji-0.ttf` | Noto Sans Symbols, Noto Sans Symbols 2, Noto Emoji (monochrome) | SIL Open Font License 1.1 |

The Noto files are fallbacks for characters none of the editor's fonts have
(★ ✕ ① ⚠ emoji…), which the browser would take from system fonts. They're
from [google/fonts](https://github.com/google/fonts), instanced at weight 400
and subset to symbol and emoji ranges with `fontTools`.
