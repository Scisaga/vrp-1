"""Rebuild pinned offline map assets, or --check their hashes and CJK glyphs.

Rebuilding requires Python and Node at development time. Runtime uses only the
embedded resources. Large downloaded font/tool inputs stay under target/.
"""
import argparse
import gzip
import hashlib
import io
import json
from pathlib import Path
import subprocess
from urllib.request import urlopen
import zipfile

ROOT = Path(__file__).resolve().parents[1]
ASSETS = ROOT / 'web/vendor/basemap'
WORK = ROOT / 'target/basemap-fonts'
ASSET_COMMIT = '028c18f713baecad011301ff7a69acc39bcc2ae7'
FONT_COMMIT = 'f8d157532fbfaeda587e826d4cd5b21a49186f7c'
FONT_URL = f'https://raw.githubusercontent.com/notofonts/noto-cjk/{FONT_COMMIT}/Sans/OTF/SimplifiedChinese/NotoSansCJKsc-Regular.otf'
FILES = {
    'NotoSansCJKsc-Regular.otf': (FONT_URL, '2c76254f6fc379fddfce0a7e84fb5385bb135d3e399294f6eeb6680d0365b74b'),
    'sdfglyph.js': ('https://maplibre.org/font-maker/sdfglyph.js', '2d5cce9e20511e3e1798a696b496cfb4fb9c55cd9cceaa73fd77849e1c3d7a64'),
    'sdfglyph.wasm': ('https://maplibre.org/font-maker/sdfglyph.wasm', 'de2986e66201499de76f21bb3a649e1f27d7d68af21cb2616dc3a7b25bade691'),
    'OFL.txt': (f'https://raw.githubusercontent.com/notofonts/noto-cjk/{FONT_COMMIT}/Sans/LICENSE', '6a73f9541c2de74158c0e7cf6b0a58ef774f5a780bf191f2d7ec9cc53efe2bf2'),
    'icons-LICENSE.md': ('https://raw.githubusercontent.com/tangrams/icons/master/LICENSE.md', '46d0ca73c10d7366ef7bf3932d8508267096393ccc9ef3a41d1b1d1fe37023f1'),
}


def download(url):
    with urlopen(url, timeout=120) as response:
        return response.read()


def protobuf(data):
    def integer(at):
        n, shift = 0, 0
        while True:
            b = data[at]
            at += 1
            n |= (b & 127) << shift
            if b < 128:
                return n, at
            shift += 7
            assert shift < 70
    at = 0
    while at < len(data):
        tag, at = integer(at)
        if tag & 7 == 0:
            value, at = integer(at)
        elif tag & 7 == 2:
            length, at = integer(at)
            value, at = data[at:at + length], at + length
        else:
            raise ValueError('Unexpected glyph protobuf wire type')
        yield tag >> 3, value


def check():
    assert len(list((ASSETS/'fonts/VRP Sans').glob('*.pbf')))==256, 'Incomplete BMP glyph set'
    manifest = json.loads((ASSETS / 'manifest.json').read_text(encoding='utf-8'))
    for entry in manifest['files']:
        assert hashlib.sha256((ASSETS / entry['path']).read_bytes()).hexdigest() == entry['sha256'], entry['path']
    # Verify actual nonempty glyph bitmaps, not just a font range's filename.
    cache = {}
    for char in '中国北京上海离线测试ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz':
        start = ord(char) // 256 * 256
        if start not in cache:
            data = gzip.decompress((ASSETS / f'fonts/VRP Sans/{start}-{start + 255}.pbf').read_bytes())
            stack = next(value for tag, value in protobuf(data) if tag == 1)
            cache[start] = {dict(protobuf(g))[1]: dict(protobuf(g)).get(2, b'') for tag, g in protobuf(stack) if tag == 3}
        assert cache[start].get(ord(char)), f'Missing actual glyph: {char}'
    print(f'Verified {len(manifest["files"])} offline assets and Chinese/Latin glyph bitmaps')


def manifest():
    (ASSETS / 'fonts/OFL-CJK.txt').write_bytes((WORK / 'OFL.txt').read_bytes())
    (ASSETS / 'sprites/LICENSE.md').write_bytes((WORK / 'icons-LICENSE.md').read_bytes())
    metadata = {'version': 'protomaps-v4-1', 'repository': 'https://github.com/protomaps/basemaps-assets',
                'commit': ASSET_COMMIT, 'font': {'repository': 'https://github.com/notofonts/noto-cjk',
                'commit': FONT_COMMIT, 'url': FONT_URL, 'sha256':FILES['NotoSansCJKsc-Regular.otf'][1]}, 'generator': {name: {'url': url, 'sha256': sha} for name, (url, sha) in FILES.items() if name.startswith('sdfglyph')},
                'files': [{'path': p.relative_to(ASSETS).as_posix(), 'sha256': hashlib.sha256(p.read_bytes()).hexdigest()}
                          for p in sorted(ASSETS.rglob('*')) if p.is_file() and p.name != 'manifest.json']}
    (ASSETS / 'manifest.json').write_text(json.dumps(metadata, ensure_ascii=False, indent=2) + '\n', encoding='utf-8')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--check', action='store_true')
    parser.add_argument('--manifest-only', action='store_true', help=argparse.SUPPRESS)
    args = parser.parse_args()
    if args.check:
        check()
        return
    if args.manifest_only:
        manifest()
        check()
        return
    WORK.mkdir(parents=True, exist_ok=True)
    ASSETS.mkdir(parents=True, exist_ok=True)
    archive = zipfile.ZipFile(io.BytesIO(download(f'https://codeload.github.com/protomaps/basemaps-assets/zip/{ASSET_COMMIT}')))
    prefix = f'basemaps-assets-{ASSET_COMMIT}/'
    for name in archive.namelist():
        relative = name.removeprefix(prefix)
        if relative == 'README.md' or relative == 'fonts/OFL.txt' or relative.startswith('fonts/Noto Sans Regular/') or relative in ['sprites/v4/light.json','sprites/v4/light.png','sprites/v4/light@2x.json','sprites/v4/light@2x.png']:
            if name.endswith('/'):
                continue
            destination = ASSETS / relative.replace('sprites/v4/', 'sprites/')
            destination.parent.mkdir(parents=True, exist_ok=True)
            destination.write_bytes(archive.read(name))
    for name, (url, sha) in FILES.items():
        path = WORK / name
        data = path.read_bytes() if path.is_file() else download(url)
        if hashlib.sha256(data).hexdigest() != sha:
            raise ValueError(f'Pinned source changed: {name}')
        path.write_bytes(data)
    subprocess.run(['node', str(ROOT / 'scripts/generate_basemap_fonts.cjs'), str(WORK), str(ASSETS / 'fonts/VRP Sans')], check=True)
    manifest()
    check()


if __name__ == '__main__':
    main()
