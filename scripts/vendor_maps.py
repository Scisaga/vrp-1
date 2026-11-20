"""Refresh pinned browser distributions; no Node or runtime CDN required."""
import base64
import hashlib
import io
import json
from pathlib import Path
import tarfile
from urllib.request import urlopen

ROOT = Path(__file__).resolve().parents[1] / 'web' / 'vendor'
PMTILES_COMMIT = '3b10e67edb65c6b04549f74c0279cef8328d859c'  # npm 4.5.0 gitHead
PACKAGES = {
    'maplibre-gl': ('6.16.0', {
        'dist/maplibre-gl.mjs': 'maplibre-gl.mjs',
        'dist/maplibre-gl-shared.mjs': 'maplibre-gl-shared.mjs',
        'dist/maplibre-gl-worker.mjs': 'maplibre-gl-worker.mjs',
        'dist/maplibre-gl.css': 'maplibre-gl.css',
        'LICENSE.txt': 'maplibre-LICENSE.txt',
    }),
    'pmtiles': ('4.5.0', {'dist/pmtiles.js': 'pmtiles.js'}),
}


def main():
    ROOT.mkdir(parents=True, exist_ok=True)
    manifest = {}
    for name, (version, files) in PACKAGES.items():
        with urlopen(f'https://registry.npmjs.org/{name}/{version}', timeout=30) as response:
            meta = json.load(response)
        with urlopen(meta['dist']['tarball'], timeout=60) as response:
            data = response.read()
        algorithm, expected = meta['dist']['integrity'].split('-', 1)
        actual = base64.b64encode(hashlib.new(algorithm, data).digest()).decode()
        if actual != expected:
            raise ValueError('package integrity mismatch')
        archive = tarfile.open(fileobj=io.BytesIO(data), mode='r:gz')
        # Only copy the explicit file allowlist; never extract arbitrary paths.
        for source, destination in files.items():
            (ROOT / destination).write_bytes(archive.extractfile('package/' + source).read())
        manifest[name] = {'version': version, 'integrity': meta['dist']['integrity']}
    with urlopen(f'https://raw.githubusercontent.com/protomaps/PMTiles/{PMTILES_COMMIT}/LICENSE', timeout=30) as response:
        license_data = response.read()
    (ROOT / 'pmtiles-LICENSE.txt').write_bytes(license_data)
    manifest['pmtiles']['license_sha256'] = hashlib.sha256(license_data).hexdigest()
    manifest['pmtiles']['license_commit'] = PMTILES_COMMIT
    (ROOT / 'manifest.json').write_text(json.dumps(manifest, indent=2) + '\n', encoding='utf-8')
    print('Vendored MapLibre 6.16.0 and PMTiles 4.5.0')


if __name__ == '__main__':
    main()
