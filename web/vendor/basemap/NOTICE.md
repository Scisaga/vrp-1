# Offline map resources

Sprites and the original Latin font resources are from
[protomaps/basemaps-assets](https://github.com/protomaps/basemaps-assets), commit
`028c18f713baecad011301ff7a69acc39bcc2ae7`. Sprites retain their MIT license in
`sprites/LICENSE.md`; original fonts retain `fonts/OFL.txt`.

The `VRP Sans` glyph stack is generated from Noto Sans CJK SC Regular, from
[notofonts/noto-cjk](https://github.com/notofonts/noto-cjk), commit
`f8d157532fbfaeda587e826d4cd5b21a49186f7c`, under `fonts/OFL-CJK.txt`.
It provides Chinese and Latin SDF glyph bitmaps through the BMP. It does not
depend on fonts installed on a browser's operating system. Glyph PBF files in
this stack are gzip compressed and served with `Content-Encoding: gzip`.

Build-time generation uses MapLibre font-maker; the JS/WASM input hashes, source
font hash, upstream versions and every output hash are in `manifest.json`.
Run `python scripts/vendor_basemap.py --check` to verify the files and actual
Chinese/Latin glyph bitmaps, or omit `--check` to rebuild with Python and Node.
Neither font-maker nor Node is needed by vrp-server at runtime.
