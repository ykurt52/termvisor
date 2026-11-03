#!/usr/bin/env python3
# Usage: mkico.py <icon.svg> <out.ico>
# Renders the app SVG at the Windows icon sizes and packs them as PNG
# entries into one .ico.
import struct, sys
import gi
gi.require_version('GdkPixbuf', '2.0')
from gi.repository import GdkPixbuf

svg, out = sys.argv[1], sys.argv[2]
sizes = [16, 20, 24, 32, 40, 48, 64, 128, 256]
pngs = []
for s in sizes:
    p = GdkPixbuf.Pixbuf.new_from_file_at_size(svg, s, s)
    ok, data = p.save_to_bufferv('png', [], [])
    pngs.append(bytes(data))
head = struct.pack('<HHH', 0, 1, len(sizes))
offset = 6 + 16 * len(sizes)
entries = b''
for s, d in zip(sizes, pngs):
    entries += struct.pack('<BBBBHHII', s % 256, s % 256, 0, 0, 1, 32, len(d), offset)
    offset += len(d)
open(out, 'wb').write(head + entries + b''.join(pngs))
