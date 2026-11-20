"""Detailed localized product blueprint, rendered by render_readme_diagrams."""


def render_blueprint(en, Diagram):
    pick = lambda zh, english: english if en else zh
    language = 'en' if en else 'zh-CN'
    ink, muted, teal, cyan, orange = '#153c4a', '#56727e', '#055668', '#0bbbe7', '#bd5d13'
    d = Diagram(language, pick('vrp-1 产品蓝图：复杂规划到真实执行', 'vrp-1 product blueprint: complex planning through actual execution'),
        pick('五阶段业务闭环：场景建模、地理准备、可解释规划、校验采用、执行重排。展开任务依赖、地图包、采用边界与版本追溯；区分保存场景、冻结输入、候选、生效计划与实际执行。',
             'Five connected stages: model scenarios, prepare geography, inspect explained plans, validate adoption, and replan during execution. Details cover task dependencies, map packages, adoption boundaries, and version lineage. Saved scenarios, frozen input, candidates, published plans, and execution facts remain distinct.'), 1676)
    d.header(pick('产品蓝图 / 业务闭环与工作区', 'PRODUCT BLUEPRINT / WORKFLOW & WORKSPACES'),
        pick('把复杂规划，连接到每一次真实执行', 'Connect complex planning to actual execution.'),
        pick('多场景协同 · 复杂约束 · 真实路网 · 可解释候选 · 执行事实驱动重排',
             'Multiple scenarios · rich constraints · real roads · explained candidates · fact-driven replanning'))

    d.rect(40, 184, 1360, 62, '#e5f3f8', '#bcdce6', 10)
    d.text(64, 222, pick('全局工作台', 'GLOBAL WORKBENCH'), 17, teal, 700)
    for x, text in zip([405, 682, 956, 1244], [pick('场景', 'Scenarios'), pick('路网包管理', 'Map packages'),
                                                    pick('任务中心', 'Task center'), pick('服务设置', 'Service settings')]):
        d.text(x, 222, text, 20, teal, 600, 'middle')
    d.text(40, 284, pick('业务主流程 / 每次求解有输入，每次采用有基准，每次执行有事实',
                       'WORKFLOW / Explicit input, adoption baselines, and execution facts'), 18, muted, 600)

    titles = [pick('场景建模', 'Model scenarios'), pick('地理准备', 'Prepare geography'),
              pick('规划与解释', 'Plan and explain'), pick('校验与采用', 'Validate and adopt'),
              pick('执行与重排', 'Execute and replan')]
    captions = ['MODEL', 'GEO', 'PLAN', 'ADOPT', 'OPERATE']
    details = [
        [pick('工单 / 执行单元 / 地点', 'Orders / units / locations'), pick('时间窗 / 多维容量', 'Windows / capacities'),
         pick('技能 / 资质 / 网点', 'Skills / levels / depots'), pick('多前置依赖 / 关联', 'Prerequisites / related orders')],
        [pick('道路矩阵与路径生成', 'Road matrices / geometry'), pick('路网来源与质量标记', 'Provenance / quality flags'),
         pick('离线底图独立准备', 'Separate offline basemaps'), pick('共享缓存与固定版本', 'Shared cache / versions')],
        [pick('多线程搜索 / 热启动', 'Parallel search / warm start'), pick('甘特与地图联动', 'Linked Gantt and map'),
         pick('未分配 / 罚分 / 风险', 'Unassigned / penalties / risks'), pick('方案对比 / 人工调整', 'Compare / adjust plans')],
        [pick('核对业务与执行基准', 'Business / execution baseline'), pick('校验载荷、容量与可达性', 'Load / capacity / reachability'),
         pick('核对依赖与明确承诺', 'Dependencies / commitments'), pick('人工确认 / 可选自动采用', 'Review / optional auto-adopt')],
        [pick('出发 / 到达 / 服务 / 完工', 'Depart / arrive / serve / finish'), pick('位置 / 装卸 / 实际载荷', 'Positions / cargo / actual loads'),
         pick('插单 / 取消 / 延迟 / 故障', 'New / cancel / delay / faults'), pick('剩余工作生成新候选', 'New remaining-work candidate')],
    ]
    for i, x in enumerate([40, 316, 592, 868, 1144]):
        d.rect(x, 310, 256, 360)
        d.rect(x, 310, 256, 5, '#ff992f' if i > 2 else cyan, 'none', 2)
        d.text(x + 20, 347, f'0{i + 1}', 17, orange if i > 2 else teal, 700, mono=True)
        d.text(x + 62, 347, captions[i], 13, muted, 600)
        d.text(x + 20, 385, titles[i], 23 if not en else 22, ink, 700)
        d.rect(x + 20, 405, 216, 112, '#f0f6f8', 'none', 8)
        for j, text in enumerate(details[i]):
            d.circle(x + 23, 546 + j * 29, 2.5, '#e58a38' if i > 2 else cyan)
            d.text(x + 34, 552 + j * 29, text, 15 if en else 17, ink)
        if i < 4:
            d.path(f'M{x + 259} 385 h14', teal, 2, True)

    # Model: a small multi-field worksheet.
    for i, text in enumerate(['ORDER-021', 'ORDER-022', 'ORDER-023']):
        y = 438 + i * 28
        d.path(f'M76 {y + 9} H258', '#d7e4e9', 1)
        d.text(76, y, text, 12, teal, mono=True)
        d.text(176, y, ['08:30', '10:00', '13:30'][i], 12, muted)
        d.circle(244, y - 4, 4, [cyan, '#56baa7', '#e59b51'][i])
    # Geography: a translucent travel-time matrix in front of a street basemap.
    d.parts.append('<defs><clipPath id="geo-map-clip"><rect x="336" y="405" width="216" height="112" rx="8"/></clipPath></defs>')
    d.parts.append('<g id="geo-illustration" clip-path="url(#geo-map-clip)">')
    d.parts.append('<title>' + pick('行 i 表示起点，列 j 表示终点。标记的单元格表示从 i 到 j 的在途时长。底图显示三条配送路径。',
                                    'Row i is the origin and column j is the destination. The marked cell gives the travel time from i to j. Three delivery routes appear on the map.') + '</title>')
    d.parts.append('<g id="geo-basemap">')
    d.rect(336, 405, 216, 112, '#eaf0ed', 'none', 8)
    for x, y, w, h in [(344, 411, 28, 23), (386, 414, 37, 20), (496, 412, 37, 24), (510, 480, 31, 29)]:
        d.rect(x, y, w, h, '#d4e6d8', 'none', 3)
    d.parts.append('<path d="M467 405 C458 429 461 445 477 463 S493 493 486 517 H502 C511 490 505 473 491 456 S473 427 483 405 Z" fill="#b9dce7"/>')
    streets = [
        'M336 441 H416 L447 426 H552', 'M336 485 H399 L421 470 H475 L514 451 H552',
        'M367 405 V442 L382 464 V517', 'M424 405 V442 L437 462 V517',
        'M515 405 V435 L533 466 V517', 'M336 507 H432 L463 493 H552',
        'M336 427 H366 L385 414 H424', 'M483 405 V426 L501 443 H552',
    ]
    for street in streets:
        d.path(street, '#c5d1cf', 5)
        d.path(street, 'white', 3)
    routes = [
        ('#43816e', 'M343 429 H355 L367 441 H416 L447 426 H515 V417 H541',
         [(343, 429), (416, 441), (498, 426), (541, 417)]),
        ('#ac744e', 'M344 500 H381 L399 485 L421 470 H453 L476 493 H513 L528 478',
         [(344, 500), (399, 485), (476, 493), (513, 493)]),
        ('#536e89', 'M346 466 L391 444 H442 L484 478 H528 L542 464',
         [(346, 466), (391, 444), (484, 478), (542, 464)]),
    ]
    for color, path, stops in routes:
        d.path(path, '#fffdfa', 4.5)
        d.path(path, color, 2)
        for x, y in stops:
            d.circle(x, y, 2.5, 'white', color, 1.4)
    # The central and southern routes meet at this junction.
    d.parts.append('<path d="M528 474.5 l3.5 3.5 -3.5 3.5 -3.5 -3.5 Z" fill="white" stroke="#465d69" stroke-width="1.4"/>')
    d.parts.append('</g>')
    # Position the grid and its notation together in map coordinates.
    d.parts.append('<g id="geo-travel-matrix" transform="translate(383 413)">')
    cell_size = 7.5
    cell_step = cell_size + 1
    places = [(0, 0), (2, 1), (3, 0), (5, 2), (6, 1), (8, 0),
              (0, 5), (2, 7), (3, 5), (5, 7), (7, 6), (8, 8)]
    # Separate the zero diagonal, then deepen the fill every three distance units.
    travel_shades = ['#a2e1e7', '#6cceda', '#3db7cb', '#1498b2',
                     '#077793', '#045c78', '#03475e']
    for row, origin in enumerate(places):
        for column, destination in enumerate(places):
            distance = abs(origin[0] - destination[0]) + abs(origin[1] - destination[1])
            shade = travel_shades[(distance + 2) // 3]
            directional_tint = ((row * 7 + column * 3) % 5) * .015
            alpha = .30 if row == column else .54 + distance / 16 * .30 + directional_tint
            d.parts.append(f'<rect x="{column * cell_step}" y="{row * cell_step}" width="{cell_size}" height="{cell_size}" rx="0.5" fill="{shade}" fill-opacity="{alpha:.2f}"/>')
    # Index guides meet the highlighted cell at column 4, row 7 (zero-based).
    entry_x, entry_y = 4 * cell_step, 7 * cell_step
    mid_x, mid_y = entry_x + cell_size / 2, entry_y + cell_size / 2
    d.parts.append(f'<path d="M0 {mid_y} H{entry_x} M{mid_x} {cell_step} V{entry_y}" fill="none" stroke="white" stroke-width="1.65" stroke-dasharray="3 2.5"/>')
    outline = f'x="{entry_x}" y="{entry_y}" width="{cell_size}" height="{cell_size}" rx="0.5" fill="none"'
    d.parts.append(f'<rect {outline} stroke="#163f50" stroke-width="2.8"/>')
    d.parts.append(f'<rect {outline} stroke="white" stroke-width="1.25"/>')
    leader = f'M{entry_x + cell_size} {entry_y} l7 -6 h4'
    d.path(leader, 'white', 3)
    d.path(leader, ink, 1.15)
    d.parts.append('<g id="geo-matrix-notation" fill="#153c4a" stroke="white" stroke-width="2.6" stroke-linejoin="round" paint-order="stroke fill" font-style="italic" font-weight="bold">')
    d.parts.append('<style>#geo-matrix-notation text { font-family: Cambria, Georgia, serif; }</style>')
    d.parts.append(f'<text x="-7" y="{mid_y + 5}" text-anchor="middle" font-size="16">i</text>')
    d.parts.append(f'<text x="{mid_x}" y="{cell_step - .5}" text-anchor="middle" font-size="15">j</text>')
    d.parts.append(f'<text x="{entry_x + cell_size + 13}" y="{entry_y - 4}" font-size="19">t<tspan dy="4" font-size="13">ij</tspan></text>')
    d.parts.append('</g></g></g>')
    # Planning: linked schedules and an illustrative optimization trace.
    for i in range(3):
        y = 428 + i * 22
        d.text(624, y + 9, f'V{i + 1:02}', 10, muted, mono=True)
        d.rect(660 + i * 16, y, 42, 14, [cyan, '#67c6cc', '#69a9c2'][i], 'none', 2)
        d.rect(710 + i * 13, y, 56, 14, [cyan, '#67c6cc', '#69a9c2'][i], 'none', 2)
    d.path('M627 503 H664 L681 497 H707 L728 489 H757 L778 483 H822', teal, 2)
    # Adoption: explicit candidate -> checks -> published plan.
    for x, label, fill, color in [(899, pick('候选', 'Candidate'), '#fff0dc', orange),
                                  (1018, pick('生效', 'Published'), '#dff5fb', teal)]:
        d.rect(x, 425, 82, 37, fill, 'none', 6)
        d.text(x + 41, 450, label, 12, color, 600, 'middle')
    d.path('M984 444 H1014', teal, 2, True)
    d.text(996, 490, pick('版本 + 事实 + 物理约束', 'Versions + facts + physical checks'), 12, muted, 500, 'middle')
    # Execution facts are separate from the proposed suffix.
    d.path('M1181 453 H1361', '#c7d8df', 3)
    for i, x in enumerate([1185, 1238, 1291, 1344]):
        color = ['#388567', '#388567', cyan, '#ff992f'][i]
        d.circle(x, 453, 9, color)
        d.text(x, 485, pick(['完成', '完成', '服务', '候选'][i], ['Done', 'Done', 'Service', 'Candidate'][i]), 11, muted, 400, 'middle')

    d.path('M1272 672 V712 H720 V672', orange, 2, True)
    d.rect(784, 696, 435, 34, '#fff4e8', '#f2d4b3', 6)
    d.text(1001, 720, pick('执行事实 → 剩余问题 → 新候选 → 再次校验采用',
                         'Facts → remaining work → candidate → validate adoption'), 15, orange, 600, 'middle')

    d.text(40, 757, pick('数据边界 / 保留每一次输入、决策与实际发生的工作',
                       'DATA LINEAGE / Retain inputs, decisions, and the work that actually happened'), 18, muted, 600)
    labels = [pick('保存的场景', 'Saved scenario'), pick('冻结输入', 'Frozen input'), pick('候选方案', 'Candidate'),
              pick('生效计划', 'Published plan'), pick('执行事实', 'Execution facts'), pick('历史快照', 'Historical snapshot')]
    notes = [pick('明确业务版本', 'Business version'), pick('问题 / 矩阵 / 参数', 'Problem / matrix / options'), pick('独立结果与解释', 'Separate result / explanation'),
             pick('采用审计与版本', 'Adoption audit / version'), pick('事件与实际载荷', 'Events / actual cargo'), pick('可查询与回看', 'Query / inspect history')]
    for i, x in enumerate([40, 270, 500, 730, 960, 1190]):
        d.rect(x, 780, 208, 84, '#e7f1f5', 'none', 8)
        d.text(x + 104, 812, labels[i], 18, teal, 600, 'middle')
        d.text(x + 104, 841, notes[i], 13 if en else 15, muted, 400, 'middle')
        if i < 5:
            d.path(f'M{x + 212} 821 h14', '#8ca9b6', 1.5, True)

    # The task graph makes operational complexity explicit, including artifacts.
    d.rect(40, 898, 872, 242)
    d.text(64, 937, pick('任务中心 / 看见准备、依赖与产物', 'Task center / preparation, dependencies, artifacts'), 24 if not en else 23, ink, 600)
    d.text(64, 970, pick('父子任务与成功依赖分开记录；各阶段保留进度、错误与资源需求。',
                       'Parent / child tasks and success dependencies are recorded separately.'), 17, muted)
    for i, (x, title, note) in enumerate([
        (64, pick('路网准备', 'Prepare roads'), 'OSM PBF'), (274, pick('矩阵固化', 'Freeze matrix'), pick('道路快照', 'Road snapshot')),
        (484, pick('规划求解', 'Solve'), pick('请求 / 结果', 'Input / result')), (694, pick('路径生成', 'Geometry'), pick('道路折线', 'Road polylines')),
    ]):
        d.rect(x, 995, 188, 70, '#edf7fa', 'none', 7)
        d.text(x + 94, 1024, title, 18, teal, 600, 'middle')
        d.text(x + 94, 1050, note, 14, muted, 400, 'middle')
        if i < 3:
            d.path(f'M{x + 191} 1030 h15', teal, 1.8, True)
    d.text(64, 1113, pick('共享 CPU / 内存额度 · 排队 / 进度 / 取消 · 失败重试 · 产物下载',
                        'Shared CPU / memory · queue / progress / cancel · retry · artifact downloads'), 17, muted)

    d.rect(936, 898, 464, 242)
    d.text(960, 937, pick('地图包 / 计算与显示各司其职', 'Map packages / compute and display'), 23, ink, 600)
    rows = [pick('路网包：PBF → 图快照 → 矩阵 / 路径', 'Road packages: PBF → graph → matrices / routes'),
            pick('底图包：PMTiles → 本地样式 / 字形 / 图标', 'Basemaps: PMTiles → local styles / fonts / icons'),
            pick('覆盖范围、来源、版本、大小与引用可查', 'Inspect coverage, provenance, versions, size, references'),
            pick('底图独立准备，不阻塞求解或采用', 'Prepare basemaps independently of solves / adoption')]
    for i, text in enumerate(rows):
        d.circle(964, 973 + i * 39, 3, cyan if i < 2 else '#87a9b7')
        d.text(978, 979 + i * 39, text, 15 if en else 17, muted)

    d.rect(40, 1170, 660, 248)
    d.text(64, 1210, pick('执行边界 / 每次采用都重新校验', 'Execution boundaries / recheck every adoption'), 24 if not en else 23, ink, 600)
    for x, w, text, color, fill in [
        (64, 176, pick('已完成事实', 'Completed facts'), '#217153', '#e5f5ed'),
        (252, 176, pick('服务中的工作', 'In-service work'), teal, '#e2f5fa'),
        (440, 236, pick('尚未完成的后缀', 'Unfinished route suffix'), orange, '#fff0df'),
    ]:
        d.pill(x, 1232, w, text, fill, color, 15)
    for i, text in enumerate([
        pick('实际位置、实际载荷与货物归属形成剩余问题的边界。', 'Actual positions, loads, and cargo ownership bound the remaining problem.'),
        pick('容量、可达性、装货先后、前置依赖与承诺继续核对。', 'Recheck capacity, reachability, load order, prerequisites, commitments.'),
        pick('持续重排与自动采用分别配置；候选通过采用后才生效。', 'Event-driven replanning and automatic adoption are separate settings.'),
    ]):
        d.text(64, 1302 + i * 36, text, 16 if en else 18, muted)

    d.rect(724, 1170, 676, 248)
    d.text(748, 1210, pick('多场景与追溯 / 业务数据不会被候选覆盖', 'Scenarios and audit / retain each decision baseline'), 24 if not en else 23, ink, 600)
    audit = [
        (pick('业务版本', 'Scenarios'), pick('各场景独立草稿；保存后产生新版本', 'Independent drafts; saving creates a new version')),
        (pick('求解快照', 'Solves'), pick('问题、矩阵、参数、结果与评分曲线', 'Problem, matrix, options, result, and score trends')),
        (pick('计划审计', 'Plans'), pick('采用人、执行基准、方案版本与变化', 'Actor, execution baseline, plan version, and changes')),
        (pick('执行历史', 'Execution'), pick('出发、到达、服务、完工与装卸事件', 'Departure, arrival, service, completion, cargo events')),
    ]
    for i, (label, text) in enumerate(audit):
        y = 1250 + i * 42
        d.pill(748, y - 20, 116, label, '#e7f1f5', teal, 15)
        d.text(884, y + 2, text, 16 if en else 17, muted)

    d.rect(40, 1450, 1360, 186, ink, 'none', 12)
    d.text(64, 1490, pick('每个场景拥有四个相互关联的工作区', 'Four connected workspaces for every scenario'), 24, 'white', 600)
    for i, text in enumerate([pick('数据与规则', 'Data and rules'), pick('规划方案', 'Plans'), pick('执行调度', 'Execution'), pick('历史记录', 'History')]):
        d.pill(64 + i * 330, 1510, 312, text, '#285362', '#e7f7fc', 20)
    d.text(64, 1582, pick('工作台 / HTTP API / CLI / Rust 库，共用同一套契约与计算引擎。',
                        'Workbench / HTTP API / CLI / Rust library share contracts and the compute engine.'), 18, '#bed4dd')
    for x, color, text in [(64, cyan, pick('生效计划', 'Published plan')), (270, '#ff992f', pick('候选变化', 'Candidate changes')),
                           (505, '#55a47b', pick('实际完成', 'Completed work')), (725, '#b3c2ce', pick('对照安排', 'Baseline schedule'))]:
        d.rect(x, 1605, 18, 7, color, 'none', 2)
        d.text(x + 28, 1617, text, 15, '#bed4dd')
    d.text(1376, 1617, pick('功能关系示意 · 非界面截图', 'Capability schematic · not a screenshot'), 13, '#aacbd6', 400, 'end')
    d.save(f'product-blueprint.{language}.svg')
