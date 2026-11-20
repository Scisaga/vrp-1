"""Render the README's self-contained, localized SVG diagrams (stdlib only)."""
from html import escape
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
OUTPUT = ROOT / 'docs' / 'assets' / 'readme'
INK = '#153c4a'
MUTED = '#56727e'
TEAL = '#055668'
CYAN = '#0bbbe7'
ORANGE = '#bd5d13'
LINE = '#d7e4e9'


class Diagram:
    def __init__(self, language, title, description, height):
        self.parts = [f'''<svg xmlns="http://www.w3.org/2000/svg" width="1440" height="{height}"
  viewBox="0 0 1440 {height}" role="img" aria-labelledby="title desc" xml:lang="{language}">
  <title id="title">{escape(title)}</title>
  <desc id="desc">{escape(description)}</desc>
  <defs>
    <pattern id="grid" width="24" height="24" patternUnits="userSpaceOnUse">
      <circle cx="1" cy="1" r=".8" fill="#d7e4e9"/>
    </pattern>
    <marker id="arrow" viewBox="0 0 10 10" refX="9" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse">
      <path d="M1 1 9 5 1 9" fill="none" stroke="{TEAL}" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round"/>
    </marker>
    <marker id="feedback" viewBox="0 0 10 10" refX="9" refY="5" markerWidth="7" markerHeight="7" orient="auto">
      <path d="M1 1 9 5 1 9" fill="none" stroke="{ORANGE}" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round"/>
    </marker>
  </defs>
  <style>
    text {{ font-family: 'Segoe UI', 'Microsoft YaHei', 'Noto Sans CJK SC', sans-serif; }}
    .mono {{ font-family: Consolas, 'DejaVu Sans Mono', monospace; }}
  </style>
  <rect width="1440" height="{height}" rx="20" fill="#f3f8fa"/>
  <rect width="1440" height="{height}" rx="20" fill="url(#grid)"/>
''']

    def rect(self, x, y, w, h, fill='white', stroke=LINE, radius=12):
        self.parts.append(f'<rect x="{x}" y="{y}" width="{w}" height="{h}" rx="{radius}" fill="{fill}" stroke="{stroke}"/>')

    def text(self, x, y, value, size=20, fill=INK, weight=400, anchor='start', mono=False):
        self.parts.append(f'<text x="{x}" y="{y}" font-size="{size}" fill="{fill}" font-weight="{weight}" text-anchor="{anchor}"' + (' class="mono"' if mono else '') + f'>{escape(value)}</text>')

    def path(self, path, color=TEAL, width=2, arrow=False, dashed=False):
        marker = 'feedback' if color == ORANGE else 'arrow'
        self.parts.append(f'<path d="{path}" fill="none" stroke="{color}" stroke-width="{width}" stroke-linecap="round" stroke-linejoin="round"' + (f' marker-end="url(#{marker})"' if arrow else '') + (' stroke-dasharray="6 6"' if dashed else '') + '/>')

    def circle(self, x, y, r, fill, stroke='none', width=1):
        self.parts.append(f'<circle cx="{x}" cy="{y}" r="{r}" fill="{fill}" stroke="{stroke}" stroke-width="{width}"/>')

    def pill(self, x, y, w, text, fill='#e9f7fb', color=TEAL, size=17):
        self.rect(x, y, w, 32, fill, 'none', 6)
        self.text(x + w / 2, y + 22, text, size, color, 600, 'middle')

    def header(self, kicker, title, subtitle):
        self.rect(40, 38, 6, 28, CYAN, 'none', 3)
        self.text(60, 59, 'vrp-1', 20, TEAL, 700)
        self.text(1400, 58, kicker, 16, MUTED, 600, 'end')
        self.text(40, 113, title, 36, INK, 700)
        self.text(40, 148, subtitle, 19, MUTED)

    def save(self, name):
        OUTPUT.mkdir(parents=True, exist_ok=True)
        (OUTPUT / name).write_text('\n'.join(self.parts) + '\n</svg>\n', encoding='utf-8')


def blueprint(en):
    from readme_blueprint import render_blueprint
    render_blueprint(en, Diagram)


def architecture(en):
    pick = lambda zh, eng: eng if en else zh
    language = 'en' if en else 'zh-CN'
    d = Diagram(language, pick('vrp-1 技术架构与计算边界', 'vrp-1 technical architecture and compute boundary'), pick(
        '工作台与业务系统通过 HTTP 访问 vrp-server。应用层负责持久化、调度、路网和离线底图。vrp-worker 通过 Protobuf IPC 运行隔离求解；CLI 也可直接调用 vrp-model。模型层完成校验和编译后进入仅依赖标准库的 vrp-core。契约唯一来源为 proto 下的文件。',
        'The workbench and business systems access vrp-server over HTTP. The application handles storage, scheduling, geography, and basemaps. vrp-worker runs isolated solves over Protobuf IPC; the CLI can also call vrp-model directly. vrp-model validates and compiles input for the standard-library-only vrp-core. Files in proto are the only contract source.'), 1060)
    d.header(pick('技术架构  /  RUNTIME & DATA FLOW', 'TECHNICAL ARCHITECTURE  /  02'),
             pick('纯计算内核，完整的应用外层', 'A pure core. A complete application around it.'),
             pick('主要调用与数据流；应用层承担网络、文件、持久化与进程管理。',
                  'Main calls and data flow. Networking, files, persistence, and process management live outside the core.'))

    d.rect(40, 182, 1360, 72, '#e5f3f8', '#bbdbe6', 10)
    d.text(64, 212, pick('唯一契约源', 'SINGLE CONTRACT SOURCE'), 13, TEAL, 700)
    d.text(64, 239, 'proto/**/*.proto', 21, TEAL, 600, mono=True)
    d.path('M370 218 H440', TEAL, 2, True)
    d.text(466, 216, 'vrp-proto', 22, TEAL, 600, mono=True)
    d.text(466, 239, pick('Prost 类型 + ProtoJSON，由 .proto 生成', 'Prost types + ProtoJSON generated from .proto'), 16, MUTED)
    d.text(1376, 213, 'engine  /  worker  /  app  /  maps', 16, TEAL, 500, 'end', True)
    d.text(1376, 239, pick('HTTP · IPC · 冻结请求 / 结果', 'HTTP · IPC · frozen requests / results'), 16, MUTED, 400, 'end')

    d.text(40, 298, pick('01 / 接入入口', '01 / ENTRY POINTS'), 16, MUTED, 600)
    d.text(1080, 298, pick('03 / 应用资源', '03 / APP RESOURCES'), 16, MUTED, 600)
    d.rect(356, 280, 680, 424, '#eaf2f5', '#c7dce4', 14)
    d.text(380, 310, pick('02 / 独立应用与隔离进程', '02 / APPLICATION & ISOLATED PROCESSES'), 16, TEAL, 600)

    for y, title, lines in [
        (336, pick('浏览器工作台', 'Browser workbench'), ['ES Modules · MapLibre', pick('HTTP · 内嵌界面资源', 'HTTP · bundled assets')]),
        (474, pick('业务系统', 'Business systems'), ['Protobuf / ProtoJSON', pick('提交任务 · 进度 · 结果', 'Submit tasks · progress · results')]),
        (612, 'vrp CLI', ['solve · solve-isolated', 'eval · bench']),
    ]:
        d.rect(40, y, 272, 110)
        d.text(60, y + 33, title, 22, INK, 600)
        d.text(60, y + 63, lines[0], 16, MUTED)
        d.text(60, y + 89, lines[1], 15, MUTED)

    d.rect(384, 336, 624, 138)
    d.text(408, 372, 'vrp-server', 25, TEAL, 700, mono=True)
    d.pill(786, 350, 198, 'Axum · Tokio', '#e9f7fb', TEAL, 16)
    d.text(408, 407, pick('HTTP API · 场景版本 · 执行投影 · 重排 / 采用', 'HTTP API · scenarios · execution projection · replan / adopt'), 18)
    d.text(408, 442, pick('有界 FIFO · 共享资源额度 · 任务事件与进度', 'Bounded FIFO · shared quotas · task events and progress'), 18, MUTED)

    d.rect(384, 540, 280, 136)
    d.text(408, 575, 'vrp-worker', 22, TEAL, 600, mono=True)
    d.text(408, 610, pick('Supervisor + 隔离求解进程', 'Supervisor + solver child'), 17)
    d.text(408, 642, pick('进度 · 取消 · 内存限制', 'Progress · cancel · memory cap'), 16, MUTED)
    d.rect(700, 540, 308, 136)
    d.text(724, 575, 'vrp-geo', 22, TEAL, 600, mono=True)
    d.text(724, 610, pick('路网进程 · PBF · 局部构图', 'Road process · PBF · local graph'), 17)
    d.text(724, 642, pick('道路矩阵 · 路径 · 来源标记', 'Matrices · geometry · provenance'), 16, MUTED)

    d.rect(1080, 336, 320, 138)
    d.text(1104, 371, pick('SQLite WAL + 本地文件', 'SQLite WAL + local files'), 22, INK, 600)
    d.text(1104, 407, pick('版本 · 任务 · 执行历史', 'Versions · tasks · execution history'), 17, MUTED)
    d.text(1104, 440, pick('冻结输入 / 结果 · 地理产物', 'Frozen inputs / results · geo artifacts'), 16, MUTED)
    d.rect(1080, 540, 320, 136)
    d.text(1104, 575, 'vrp-basemap', 22, TEAL, 600, mono=True)
    d.text(1104, 610, pick('PMTiles 提取 / 验证 · 内置资源', 'PMTiles extraction / validation'), 17)
    d.text(1104, 642, pick('仅用于显示，与路网计算分离', 'Display only; separate from routing'), 16, MUTED)

    d.path('M312 389 H384', TEAL, 2, True)
    d.path('M312 529 H334 V433 H384', TEAL, 2, True)
    d.path('M1008 402 H1080', TEAL, 2, True)
    d.path('M524 474 V540', TEAL, 2, True)
    d.rect(444, 496, 160, 26, '#eaf2f5', 'none', 2)
    d.text(524, 515, 'Protobuf IPC', 14, TEAL, 600, 'middle')
    d.path('M854 474 V540', TEAL, 2, True)
    d.text(875, 514, pick('地理任务', 'Geo tasks'), 14, MUTED)
    d.path('M1008 448 H1058 V608 H1080', TEAL, 2, True)
    # Isolated CLI mode joins the supervisor; direct mode bypasses the service.
    d.path('M312 667 H348 V608 H384', TEAL, 2, True)

    d.rect(356, 754, 1044, 238, '#e5f3f6', '#b9d6e0', 14)
    d.text(792, 785, pick('04 / 输入编译与求解', '04 / COMPILE AND SOLVE'), 16, TEAL, 600)
    d.rect(384, 810, 318, 150)
    d.text(408, 849, 'vrp-model', 25, TEAL, 600, mono=True)
    d.text(408, 886, pick('校验 · 未知边估算 · 编译', 'Validate · estimate · compile'), 18)
    d.text(408, 922, pick('协议 ↔ 内部模型 / 结果', 'Protocol ↔ internal model / result'), 17, MUTED)
    d.rect(768, 810, 604, 150, INK, 'none', 10)
    d.text(792, 849, 'vrp-core', 25, 'white', 700, mono=True)
    d.pill(1212, 828, 136, 'STD ONLY', '#285362', '#a4e9fa', 15)
    d.text(792, 887, pick('多线程搜索 · 路线级增量评分 · 独立全量 oracle', 'Parallel search · incremental scoring · independent oracle'), 18, '#e6f3f7')
    d.text(792, 926, pick('无 I/O · 无 async · 无全局状态 · 不依赖协议层', 'No I/O · no async · no global state · no protocol dependency'), 17, '#a9d5e1')

    d.path('M524 676 V810', TEAL, 2, True)
    d.rect(432, 722, 184, 26, '#f3f8fa', 'none', 2)
    d.text(524, 741, 'SolveRequest', 15, TEAL, 600, 'middle', True)
    d.path('M176 722 V885 H384', TEAL, 2, True)
    d.rect(182, 797, 154, 28, '#f3f8fa', 'none', 2)
    d.text(194, 817, pick('直接调用', 'Direct solve'), 16, MUTED)
    d.path('M702 884 H768', TEAL, 2, True)
    d.text(735, 865, pick('模型', 'Model'), 13, TEAL, 500, 'middle')

    d.text(40, 1030, pick('实线：主要调用 / 数据流，非完整 crate 依赖图。', 'Lines show main calls / data flow, not every crate dependency.'), 16, MUTED)
    d.text(1400, 1030, pick('计算内核边界以 docs/architecture.md 为准', 'Core boundary: docs/architecture.md'), 16, TEAL, 500, 'end')
    d.save(f'architecture.{language}.svg')


if __name__ == '__main__':
    for english in [False, True]:
        blueprint(english)
        architecture(english)
    print('Rendered 4 localized SVG diagrams in docs/assets/readme/')
