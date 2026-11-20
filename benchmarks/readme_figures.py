"""Render bilingual README figures solely from published synthetic evidence."""
import argparse
import hashlib
import json
from pathlib import Path
import statistics

import matplotlib
matplotlib.use('Agg')
import matplotlib.pyplot as plt
import numpy as np

COLORS = {'vrp0': '#b65c35', 'vrp1': '#087f8c'}
MODES = {'cold': ('Cold construction', '冷建解'), 'continue': ('Same initial routes', '相同初始路线继续优化')}
NAMES = {
    'a01-basic-50': ('Basic delivery', '基础配送'),
    'a02-dense-200': ('Dense city', '城市密集'),
    'a03-regional-480': ('Four depots', '跨区四网点'),
    'a04-growth-1000': ('Regional growth', '规模增长'),
    'a05-skills-levels-200': ('Skills + qualifications + capacity', '技能、资质与容量'),
    'a06-tight-wait-capacity-50': ('Windows + waits + capacity', '紧窗、等待与容量'),
}


def fingerprint(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def curve_band(runs, count, budget_ms, expected):
    """Actual step functions; all configured seeds must be observed and feasible."""
    times = np.array(sorted({0.0, float(budget_ms)} | {e[0] for r in runs for e in r['incumbents']}))
    data = []
    for seed in expected:
        run = next((r for r in runs if r['seed'] == seed), None)
        values = np.full(len(times), np.nan)
        if run and run['status'] == 'succeeded' and run['incumbents']:
            events = np.array(run['incumbents'], dtype=float)
            ix = np.searchsorted(events[:, 0], times, side='right') - 1
            available = ix >= 0
            current = events[np.maximum(ix, 0)]
            valid = available & (current[:, 1] == 0) & (current[:, 2] == 0) & (current[:, 4] == count)
            values[valid] = -current[valid, 3]
        data.append(values)
    data = np.array(data)
    all_valid = np.all(np.isfinite(data), axis=0)
    bands = np.full((3, len(times)), np.nan)
    if all_valid.any():
        bands[:, all_valid] = [np.median(data[:, all_valid], axis=0),
                              np.min(data[:, all_valid], axis=0), np.max(data[:, all_valid], axis=0)]
    # Remove only consecutive identical triples: retain every actual change and gap.
    same = np.all((bands[:, 1:] == bands[:, :-1]) | (np.isnan(bands[:, 1:]) & np.isnan(bands[:, :-1])), axis=0)
    keep = np.r_[True, ~same]
    keep[-1] = True
    return times[keep] / 1000, bands[:, keep]


def render(summary_path, preview_dir=None):
    summary_path = Path(summary_path).resolve()
    folder = summary_path.parent
    traces_path = folder/'traces.json'
    summary = json.loads(summary_path.read_text(encoding='utf-8'))
    traces = json.loads(traces_path.read_text(encoding='utf-8'))
    config = summary['versions']['config']
    if summary['missing_records'] or any(s.get('private') or s.get('request_file') for s in config['scenarios']):
        raise ValueError('README figures require a complete public synthetic campaign')
    specs = [s for s in config['scenarios'] if s['suite'] == 'common']
    rows = {(r['scenario'], r['mode'], r['engine']): r for r in summary['instances']}
    pairs = {(r['scenario'], r['mode']): r for r in summary['comparisons']}
    sources = {'summary.json': fingerprint(summary_path), 'traces.json': fingerprint(traces_path)}
    metadata = {'Date': None, 'Creator': 'vrp-1 / benchmarks/readme_figures.py',
                'Description': json.dumps(sources, sort_keys=True)}
    files = []

    def save(fig, name, lang):
        name = f'readme-{name}.{lang}'
        fig.canvas.draw()
        fig.set_layout_engine('none')
        fig.savefig(folder/(name+'.svg'), metadata=metadata)
        files.append(name+'.svg')
        if preview_dir:
            preview = Path(preview_dir); preview.mkdir(parents=True, exist_ok=True)
            fig.savefig(preview/(name+'.png'), dpi=120)
        plt.close(fig)

    for lang in ('en', 'zh-CN'):
        zh = lang == 'zh-CN'
        plt.rcParams.update({'font.family': 'Microsoft YaHei' if zh else 'DejaVu Sans',
                             'font.size': 10, 'svg.fonttype': 'none', 'svg.hashsalt': 'vrp-1-readme',
                             'axes.spines.top': False, 'axes.spines.right': False})
        labels = [f"{s['id'][:3]} · {NAMES[s['id']][int(zh)]}" for s in specs]
        size_labels = [f"{label}\n{s['tickets']} {'单' if zh else 'orders'} / {s['agents']} {'车' if zh else 'vehicles'}"
                       for label, s in zip(labels, specs)]
        y = np.arange(len(specs))

        fig, ax = plt.subplots(figsize=(12, 5.5), layout='constrained')
        for mode, color, offset in [('cold', '#087f8c', -.18), ('continue', '#6378a5', .18)]:
            stats = [pairs[(s['id'], mode)]['cost_improvement_percent'] for s in specs]
            med = np.array([d['median'] if d['median'] is not None else np.nan for d in stats])
            low = np.array([d['min'] if d['min'] is not None else np.nan for d in stats])
            high = np.array([d['max'] if d['max'] is not None else np.nan for d in stats])
            ax.barh(y+offset, med, .3, color=color, label=MODES[mode][int(zh)],
                    xerr=[med-low, high-med], capsize=3, error_kw={'elinewidth': 1, 'alpha': .8})
            for i, value in enumerate(med):
                if np.isfinite(value): ax.text(high[i]+.5, i+offset, f'{value:+.2f}%', va='center', fontsize=9)
        ax.set_yticks(y, size_labels); ax.invert_yaxis(); ax.set_xlim(right=ax.get_xlim()[1]+3)
        ax.set_xlabel('vrp-1 行驶成本下降（%，配对中位数；误差线为五种子最小–最大）' if zh else
                      'Lower vrp-1 travel cost (paired median %; whiskers: min–max across five seeds)')
        ax.set_title(f"相同 {config['budget_ms']/1000:g} 秒预算：最终全派可行方案的行驶成本" if zh else
                     f"Same {config['budget_ms']/1000:g}-second budget: final travel cost of fully feasible plans", pad=14)
        ax.grid(axis='x', alpha=.18); ax.set_axisbelow(True); ax.legend(loc='lower right')
        save(fig, 'quality-summary', lang)

        for mode in ('cold', 'continue'):
            fig, axes = plt.subplots(3, 2, figsize=(12, 9.6), layout='constrained')
            for ax, spec, label in zip(axes.flat, specs, size_labels):
                max_time = config['budget_ms']/1000
                for engine in ('vrp0', 'vrp1'):
                    runs = [r for r in traces if (r['scenario'], r['mode'], r['engine']) == (spec['id'], mode, engine)]
                    times, band = curve_band(runs, spec['tickets'], config['budget_ms'], config['seeds'])
                    max_time = max(max_time, times[-1])
                    band = band/3600
                    ax.step(times, band[0], where='post', color=COLORS[engine], linewidth=1.7,
                            linestyle='--' if engine == 'vrp0' else '-', label=engine.replace('vrp','vrp-'))
                    ax.fill_between(times, band[1], band[2], step='post', color=COLORS[engine], alpha=.13)
                ax.set_title(label, fontsize=11); ax.set_xlim(0, max_time)
                ax.axvline(config['budget_ms']/1000, color='#888888', linestyle=':', linewidth=.8)
                ax.set_ylabel('行驶小时（越低越好）' if zh else 'Travel hours (lower is better)')
                ax.set_xlabel('求解秒数（含预处理、建解）' if zh else 'Solve seconds (incl. preprocessing + construction)')
                ax.grid(alpha=.18); ax.legend(fontsize=9, loc='upper right')
            fig.suptitle((f'{MODES[mode][1]}：真实最佳方案事件，中位数及五种子范围\n全部种子全派且无硬违约后才显示成本；各场景纵轴独立' if zh else
                          f'{MODES[mode][0]}: actual incumbent events, median and five-seed range\nCost starts only when all seeds are fully feasible; independent y scales'), fontsize=13)
            save(fig, 'quality-'+mode, lang)

        fig, axes = plt.subplots(1, 2, figsize=(12, 5.8), sharey=True, layout='constrained')
        for ax, mode in zip(axes, ('cold', 'continue')):
            for engine, offset, marker in [('vrp0', -.13, 's'), ('vrp1', .13, 'o')]:
                for i, spec in enumerate(specs):
                    entries = pairs[(spec['id'], mode)]['equal_quality']
                    values = [e['first_ms'][engine]/1000 for e in entries if e['first_ms'].get(engine) is not None]
                    if values:
                        med = statistics.median(values)
                        ax.errorbar(med, i+offset, xerr=[[med-min(values)], [max(values)-med]],
                                    fmt=marker, color=COLORS[engine], capsize=3, label=engine.replace('vrp','vrp-') if i == 0 else None)
                    if len(values) != len(entries):
                        ax.text(config['budget_ms']/1000, i+offset, f' {len(values)}/{len(entries)}', va='center', fontsize=8)
            ax.set_xscale('log'); ax.set_xlim(.001, max(50, config['budget_ms']/1000*1.5))
            ax.axvline(config['budget_ms']/1000, color='#888888', linestyle=':', linewidth=.8)
            ax.set_title(MODES[mode][int(zh)]); ax.grid(axis='x', which='both', alpha=.15)
            ax.set_xlabel('首次达到目标的求解秒数（对数轴）' if zh else 'Solve seconds to target (log scale)')
            ax.legend(loc='lower right')
        axes[0].set_yticks(y, labels); axes[0].invert_yaxis()
        fig.suptitle('达到同等行驶成本：每个配对取双方较差的最终成本为目标\n事后目标，非预设业务门槛；点为中位数，横线为种子范围' if zh else
                     'Time to equal travel cost: worse final cost in each paired run\nPost hoc target, not a preset business threshold; median and seed range', fontsize=12)
        save(fig, 'equal-quality', lang)

        fig, axes = plt.subplots(1, 2, figsize=(12, 5.8), sharey=True, layout='constrained')
        for ax, key, scale in zip(axes, ('cpu_seconds', 'peak_tree_commit_bytes'), (1, 1024**2)):
            for engine, offset, marker in [('vrp0', -.13, 's'), ('vrp1', .13, 'o')]:
                stats = [rows[(s['id'], 'cold', engine)][key] for s in specs]
                med = np.array([d['median']/scale for d in stats])
                low = np.array([d['min']/scale for d in stats]); high = np.array([d['max']/scale for d in stats])
                ax.errorbar(med, y+offset, xerr=[med-low, high-med], fmt=marker, color=COLORS[engine], capsize=3, label=engine.replace('vrp','vrp-'))
            ax.grid(axis='x', which='both', alpha=.15)
        axes[0].legend(loc='upper left')
        axes[0].set_yticks(y, labels); axes[0].invert_yaxis(); axes[0].set_xlim(left=0)
        axes[0].set_title('实测 CPU 秒 / 指令' if zh else 'Measured CPU seconds / command')
        axes[0].set_xlabel('中位数及五种子范围' if zh else 'Median and five-seed range')
        axes[1].set_xscale('log')
        axes[1].set_title('峰值提交内存 MiB（对数轴）' if zh else 'Peak committed MiB (log scale)')
        axes[1].set_xlabel('Java：含预热的整组峰值；Rust：单进程峰值' if zh else
                           'Java: group incl. warmup; Rust: one process')
        fig.suptitle('冷建解资源实测：单 CPU 亲和性、2 GiB 进程树限制\nJava 与 Rust 的内存生命周期不同，不计算部署资源倍率' if zh else
                     'Cold construction resources: one CPU affinity, 2 GiB process-tree limit\nMemory lifetimes differ; these are not deployed-service resource ratios', fontsize=12)
        save(fig, 'resources', lang)

    manifest = {'sources': sources, 'renderer_sha256': fingerprint(Path(__file__)), 'matplotlib': matplotlib.__version__,
                'figures': {name: fingerprint(folder/name) for name in files},
                'method': 'Actual step events; no interpolation; all configured seeds required for feasible cost curves; independent instance scales.'}
    (folder/'readme-figures.json').write_text(json.dumps(manifest, ensure_ascii=False, indent=2)+'\n', encoding='utf-8')
    return manifest


if __name__ == '__main__':
    parser = argparse.ArgumentParser()
    parser.add_argument('--summary', default='docs/benchmarks/round-1/summary.json')
    parser.add_argument('--preview-dir')
    args = parser.parse_args()
    result = render(args.summary, args.preview_dir)
    print(json.dumps({'figures': len(result['figures']), 'sources': result['sources']}))
