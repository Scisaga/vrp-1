"""Synthetic workbench input shared by browser acceptance checks."""
import base64
from datetime import date
import math
import struct


def synthetic_matrix(locations):
    """Complete local estimates; browser checks never need a public road source."""
    count = len(locations)
    durations, distances = bytearray(count * count * 4), bytearray(count * count * 4)
    sources = bytearray(count * count)
    for i, start in enumerate(locations):
        for j, end in enumerate(locations):
            index = i * count + j
            # A fixed local grid approximation is sufficient for UI acceptance.
            latitude = math.radians((start['lat'] + end['lat']) / 2)
            meters = round(111_195 * (abs(start['lat'] - end['lat'])
                                     + abs(start['lng'] - end['lng']) * math.cos(latitude)))
            struct.pack_into('<I', distances, index * 4, meters)
            struct.pack_into('<I', durations, index * 4, round(meters / (30 / 3.6)))
            sources[index] = 0 if i == j else 2  # MODEL, synthetic estimate.
    encode = lambda value: base64.b64encode(value).decode('ascii')
    return {'durations': encode(durations), 'distances': encode(distances), 'sources': encode(sources)}


def workbench_scenario(scene_id, matrix_ready=True):
    problem = {
        'id': scene_id,
        'horizon': {'startLocal': date.today().isoformat() + 'T00:00:00', 'timezone': 'Asia/Shanghai'},
        'locations': [{'id': 'DEPOT', 'lng': 121.47, 'lat': 31.23}],
        'capacityDimensions': [{'name': '件数', 'unit': '件', 'penaltyUnit': '1'}],
        'agents': [], 'tickets': [],
    }
    display = {'agents': {}, 'tickets': {}, 'locations': {'DEPOT': {'name': '服务中心', 'address': '测试地址 · 中心网点'}}}
    for i in range(3):
        agent_id = f'V{i + 1}'
        problem['agents'].append({
            'id': agent_id, 'resourceId': f'{scene_id}-{agent_id}',
            'startTime': '28800', 'endTime': '64800', 'capacity': ['20'],
            'skills': ['安装', '巡检'], 'maxTickets': 8, 'fixedCost': '50',
        })
        display['agents'][agent_id] = {'name': ['一组 · 东区', '二组 · 西区', '三组 · 机动'][i]}
    for i in range(12):
        location_id, ticket_id = f'L{i + 1}', f'ORDER-{i + 1:02}'
        problem['locations'].append({'id': location_id, 'lng': 121.43 + (i % 4) * .025, 'lat': 31.2 + (i // 4) * .022})
        display['locations'][location_id] = {'name': f'服务点 {i + 1:02}', 'address': f'测试地址 · {i + 1} 号'}
        problem['tickets'].append({
            'id': ticket_id, 'location': i + 1, 'duration': str(1200 + (i % 3) * 600),
            'earliestStart': str(28800 + (i % 3) * 3600), 'latestStart': str(43200 + (i % 4) * 1800),
            'latestEnd': '61200', 'skillsRequired': ['巡检' if i % 2 else '安装'], 'demand': ['0'],
        })
        display['tickets'][ticket_id] = {'name': f'{"设备巡检" if i % 2 else "设备安装"} · {i + 1:02}'}
    if matrix_ready:
        problem['matrices'] = [synthetic_matrix(problem['locations'])]
    return {
        'scenarioId': scene_id, 'name': '工作台验收',
        'request': {'problem': problem, 'options': {'preset': 0, 'threads': 1, 'searchStrategy': 3}},
        'display': display,
    }
