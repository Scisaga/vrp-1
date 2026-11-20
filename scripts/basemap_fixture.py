"""Small, synthetic PMTiles v3 archive for offline tests (no downloaded maps)."""
import gzip
import json
import math
import struct


def varint(n):
    value = bytearray()
    while n > 127:
        value.append((n & 127) | 128)
        n >>= 7
    return bytes(value + bytes([n]))


def field(n, value):
    if isinstance(value, bytes):
        return varint(n * 8 + 2) + varint(len(value)) + value
    return varint(n * 8) + varint(value)


def tile_id(z, x, y):
    side, distance, step = 1 << z, 0, (1 << z) >> 1
    while step:
        rx, ry = bool(x & step), bool(y & step)
        distance += step * step * ((3 * rx) ^ ry)
        if not ry:
            if rx:
                x, y = side - 1 - x, side - 1 - y
            x, y = y, x
        step >>= 1
    return ((1 << (2 * z)) - 1) // 3 + distance


def vector_tile():
    def feature(kind, geometry, tags=()):
        return field(1, 1) + field(2, b''.join(varint(x) for x in tags)) + field(3, kind) + field(4, b''.join(varint(x) for x in geometry))

    # Geometry is deliberately generic: every local tile shows Chinese text,
    # an earth polygon, a road and a hospital icon. It contains no real places.
    polygon = feature(3, [9, 0, 0, 26, 8192, 0, 0, 8192, 8191, 0, 15])
    road = feature(2, [9, 0, 4096, 10, 8192, 0])
    points = [feature(1, [9, x * 2, y * 2], [0, 0, 1, 1]) for x in [256,1024,2048,3072,3840] for y in [256,1024,2048,3072,3840]]
    poi = feature(1, [9, 3000, 3000], [0, 0, 1, 1])
    out = b''
    for name, features in [('earth', [polygon]), ('water', []), ('landuse', []),
                           ('buildings', []), ('roads', [road]), ('places', points), ('pois', [poi])]:
        layer = field(15, 2) + field(1, name.encode()) + b''.join(field(2, f) for f in features)
        layer += field(3, b'name:zh') + field(3, b'kind')
        layer += field(4, field(1, '离线测试'.encode())) + field(4, field(1, b'hospital' if name == 'pois' else b'city'))
        layer += field(5, 4096)
        out += field(3, layer)
    return out


def archive(maxzoom=15, bounds=(121.1, 30.9, 121.8, 31.6), leaves=False):
    west, south, east, north = bounds
    ids = set()
    for z in range(maxzoom + 1):
        side = 1 << z
        xx = lambda lng: min(side - 1, int((lng + 180) / 360 * side))
        yy = lambda lat: min(side - 1, int((1 - math.asinh(math.tan(math.radians(lat))) / math.pi) / 2 * side))
        for y in range(yy(north), yy(south) + 1):
            for x in range(xx(west), xx(east) + 1):
                ids.add(tile_id(z, x, y))
    ids = sorted(ids)
    tile = vector_tile()
    metadata = json.dumps({'vector_layers': [{'id': n} for n in ['earth', 'water', 'landuse', 'buildings', 'roads', 'places', 'pois']],
                           'name': 'Synthetic offline map', 'attribution': 'Synthetic fixture · no real map data'}).encode()
    directory = varint(len(ids))
    previous = 0
    for value in ids:
        directory += varint(value - previous)
        previous = value
    directory += varint(1) * len(ids) + varint(len(tile)) * len(ids) + varint(1) * len(ids)
    if leaves:
        leaf = gzip.compress(directory, mtime=0)
        root = gzip.compress(varint(1) + varint(ids[0]) + varint(0) + varint(len(leaf)) + varint(1), mtime=0)
    else:
        root, leaf = gzip.compress(directory, mtime=0), b''
    metadata = gzip.compress(metadata, mtime=0)
    header = bytearray(127)
    header[:8] = b'PMTiles\x03'
    metadata_offset = 127 + len(root)
    leaf_offset = metadata_offset + len(metadata)
    data_offset = leaf_offset + len(leaf)
    for offset, value in [(8, 127), (16, len(root)), (24, metadata_offset), (32, len(metadata)),
                          (40, leaf_offset), (48, len(leaf)), (56, data_offset), (64, len(tile)),
                          (72, len(ids)), (80, len(ids)), (88, 1)]:
        struct.pack_into('<Q', header, offset, value)
    header[96:102] = bytes([1, 2, 1, 1, 0, maxzoom])
    for offset, coord in [(102, west), (106, south), (110, east), (114, north), (119, (west + east) / 2), (123, (south + north) / 2)]:
        struct.pack_into('<i', header, offset, round(coord * 1e7))
    header[118] = 12
    return bytes(header) + root + metadata + leaf + tile
