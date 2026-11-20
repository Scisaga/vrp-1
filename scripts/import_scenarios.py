"""Offline v1 replay conversion and idempotent native scenario upload.

All source bytes, generated scenarios and reports stay in the private corpus.
No routing provider is contacted. --server is optional; without it only exports.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import time
from urllib.error import HTTPError
from urllib.request import Request, build_opener, ProxyHandler

sys.dont_write_bytecode = True
from organize_corpus import describe


def main():
    root = Path(__file__).resolve().parents[1]
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--source', type=Path, required=True, help='the scenarios directory')
    parser.add_argument('--output', type=Path, default=root / 'corpus' / 'workbench')
    parser.add_argument('--vrp', type=Path, default=root / 'target' / 'release' / ('vrp.exe' if os.name == 'nt' else 'vrp'))
    parser.add_argument('--server', help='local/native API base URL')
    parser.add_argument('--crs-overrides', type=Path, help='private JSON: relative filename -> GCJ-02 or WGS84')
    args = parser.parse_args()
    source = args.source.resolve(strict=True)
    output = args.output.resolve()
    if output == source or output.is_relative_to(source) or source.is_relative_to(output):
        parser.error('source and output must not overlap')
    output.mkdir(parents=True, exist_ok=True)
    overrides = json.loads(args.crs_overrides.read_text(encoding='utf-8-sig')) if args.crs_overrides else {}
    local = build_opener(ProxyHandler({}))

    def api(path, data=None):
        headers = {'Accept': 'application/json', 'Content-Type': 'application/json'}
        if os.environ.get('VRP_API_TOKEN'):
            headers['Authorization'] = 'Bearer ' + os.environ['VRP_API_TOKEN']
        request = Request(args.server.rstrip('/') + path, data=None if data is None else json.dumps(data, ensure_ascii=False).encode('utf-8'), headers=headers)
        with local.open(request, timeout=120) as response:
            return json.load(response)

    records, seen = [], {}
    for file in sorted(source.rglob('*.json')):
        if file.is_symlink() or not file.resolve().is_relative_to(source):
            continue
        data = file.read_bytes()
        sha = hashlib.sha256(data).hexdigest()
        relative = file.relative_to(source).as_posix()
        info, _ = describe(data, '.json')
        record = {'source': relative, 'sha256': sha, **info}
        records.append(record)
        raw = output / 'raw' / info['kind'] / (sha + '.json')
        raw.parent.mkdir(parents=True, exist_ok=True)
        if not raw.exists(): raw.write_bytes(data)
        if info['kind'] != 'request':
            record['status'] = 'catalogued'
            continue
        if not info['within_target']:
            record['status'] = 'outside_target'
            continue
        # Coordinate overrides alter the converted snapshot, so its identity must
        # differ while repeated conversion of the same configuration stays stable.
        crs = overrides.get(relative)
        identity = hashlib.sha256((sha + ':' + (crs or 'legacy-default-v1')).encode()).hexdigest()
        if identity in seen:
            record.update(status='duplicate', scenario_id=seen[identity])
            continue
        scene_id = 'legacy-' + identity[:24]
        seen[identity] = scene_id
        native = output / 'native' / (scene_id + '.json')
        native.parent.mkdir(parents=True, exist_ok=True)
        record.update(scenario_id=scene_id, native=native.relative_to(output).as_posix())
        if not native.exists():
            command = [str(args.vrp.resolve()), 'export-scene', str(file), str(native), '--id', scene_id, '--source-name', relative, '--sha256', sha]
            if crs: command.extend(['--source-crs', crs])
            run = subprocess.run(command, capture_output=True, timeout=180, creationflags=subprocess.CREATE_NO_WINDOW if os.name == 'nt' else 0)
            if run.returncode:
                record.update(status='conversion_failed', error=run.stderr.decode('utf-8', errors='replace').strip())
                continue
        record['status'] = 'exported'
        if args.server:
            try:
                existing = api('/v1/scenarios/' + scene_id + '/metadata')
                record.update(status='already_present', version=existing['version'])
            except HTTPError as error:
                if error.code != 404:
                    record.update(status='upload_failed', error=f'HTTP {error.code}')
                    continue
                try:
                    saved = api('/v1/scenarios', json.loads(native.read_text(encoding='utf-8')))
                    record.update(status='imported', version=saved['version'])
                except HTTPError as error:
                    record.update(status='upload_failed', error=error.read().decode('utf-8', errors='replace'))
    report = {'source': str(source), 'records': records}
    report_file = output / f'import-{time.time_ns()}.json'
    report_file.write_text(json.dumps(report, ensure_ascii=False, indent=2), encoding='utf-8')
    from collections import Counter
    print(json.dumps(dict(Counter(r['status'] for r in records)), ensure_ascii=True))
    print('Private report:', report_file)
    return 1 if any(r['status'] in ('conversion_failed', 'upload_failed') for r in records) else 0


if __name__ == '__main__':
    raise SystemExit(main())
