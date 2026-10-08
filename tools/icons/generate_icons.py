#!/usr/bin/env python3
"""Generate original Racc Connect icons using only the Python standard library."""
from __future__ import annotations

import argparse
import math
import struct
import zlib
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
ACCENT = (91, 95, 220, 255)
FACE = (239, 238, 248, 255)
OUTLINE = (35, 32, 47, 255)
INNER_EAR = (192, 103, 179, 255)
EYE = (255, 255, 255, 255)
NOSE = (130, 91, 216, 255)
CONNECTED = (84, 205, 139, 255)
DISCONNECTED = (153, 157, 169, 255)

# One geometry source feeds both the SVG masters and the raster output.
HEAD_OUTER = [(0.23,0.31),(0.28,0.17),(0.43,0.26),(0.50,0.24),(0.57,0.26),(0.72,0.17),(0.77,0.31),(0.83,0.48),(0.80,0.68),(0.69,0.82),(0.50,0.88),(0.31,0.82),(0.20,0.68),(0.17,0.48)]
FACE_INNER = [(0.265,0.32),(0.30,0.22),(0.43,0.30),(0.50,0.28),(0.57,0.30),(0.70,0.22),(0.735,0.32),(0.78,0.49),(0.75,0.67),(0.66,0.78),(0.50,0.84),(0.34,0.78),(0.25,0.67),(0.22,0.49)]
LEFT_EAR = [(0.30,0.25),(0.33,0.24),(0.40,0.30),(0.35,0.29)]
RIGHT_EAR = [(0.67,0.29),(0.73,0.24),(0.70,0.33)]
MASK = [(0.20,0.52),(0.32,0.39),(0.45,0.44),(0.50,0.38),(0.55,0.44),(0.68,0.39),(0.80,0.52),(0.73,0.64),(0.62,0.59),(0.50,0.68),(0.38,0.59),(0.27,0.64)]
NOSE_SHAPE = [(0.46,0.65),(0.50,0.62),(0.54,0.65),(0.50,0.69)]
MOUTH_CENTER = [(0.495,0.69),(0.505,0.69),(0.505,0.75),(0.495,0.75)]
MOUTH_LEFT = [(0.495,0.73),(0.50,0.75),(0.44,0.77),(0.42,0.75)]
MOUTH_RIGHT = [(0.505,0.73),(0.58,0.75),(0.56,0.77),(0.50,0.75)]
EYES = [(0.39,0.50),(0.61,0.50)]


def _svg_polygon(points: list[tuple[float, float]], color: str) -> str:
    coords = [(x * 64, y * 64) for x, y in points]
    path = "M " + " L ".join(f"{x:.2f} {y:.2f}" for x, y in coords) + " Z"
    return f'<path d="{path}" fill="{color}"/>'


def svg_document(template: bool = False, state: str | None = None) -> str:
    base = "" if template else '<rect x="2" y="2" width="60" height="60" rx="16" fill="#5b5fdc"/>'
    dark = "#000000" if template else "#23202f"
    face = "#000000" if template else "#efeff8"
    parts = [base, _svg_polygon(HEAD_OUTER, dark), _svg_polygon(FACE_INNER, face)]
    if not template:
        parts.extend((_svg_polygon(LEFT_EAR, "#c067b3"), _svg_polygon(RIGHT_EAR, "#c067b3")))
    parts.append(_svg_polygon(MASK, dark))
    if not template:
        for x, y in EYES:
            parts.append(f'<circle cx="{x * 64:.2f}" cy="{y * 64:.2f}" r="2.24" fill="#ffffff"/>')
            parts.append(f'<circle cx="{x * 64:.2f}" cy="{y * 64:.2f}" r="0.90" fill="{dark}"/>')
        parts.append(_svg_polygon(NOSE_SHAPE, "#825bd8"))
        parts.extend((_svg_polygon(MOUTH_CENTER, dark), _svg_polygon(MOUTH_LEFT, dark), _svg_polygon(MOUTH_RIGHT, dark)))
    if state:
        color = "#54cd8b" if state == "connected" else "#999da9"
        parts.append(f'<circle cx="51.84" cy="51.84" r="8.32" fill="#17151f"/><circle cx="51.84" cy="51.84" r="5.76" fill="{color}"/>')
    return '<svg xmlns="http://www.w3.org/2000/svg" width="64" height="64" viewBox="0 0 64 64">' + "".join(parts) + "</svg>\n"


class Canvas:
    def __init__(self, width: int, height: int) -> None:
        self.width, self.height = width, height
        self.pixels = bytearray(width * height * 4)

    def pixel(self, x: int, y: int, color: tuple[int, int, int, int]) -> None:
        if 0 <= x < self.width and 0 <= y < self.height:
            index = (y * self.width + x) * 4
            self.pixels[index:index + 4] = bytes(color)

    def span(self, y: int, left: int, right: int, color: tuple[int, int, int, int]) -> None:
        if not 0 <= y < self.height:
            return
        left, right = max(0, left), min(self.width - 1, right)
        if left <= right:
            data = bytes(color) * (right - left + 1)
            start = (y * self.width + left) * 4
            self.pixels[start:start + len(data)] = data

    def polygon(self, points: list[tuple[float, float]], color: tuple[int, int, int, int]) -> None:
        points = [(x * self.width, y * self.height) for x, y in points]
        first = max(0, int(math.floor(min(y for _, y in points))))
        last = min(self.height - 1, int(math.ceil(max(y for _, y in points))))
        for y in range(first, last + 1):
            scan = y + 0.5
            intersections = []
            for index, (x1, y1) in enumerate(points):
                x2, y2 = points[(index + 1) % len(points)]
                if (y1 <= scan < y2) or (y2 <= scan < y1):
                    intersections.append(x1 + (scan - y1) * (x2 - x1) / (y2 - y1))
            intersections.sort()
            for index in range(0, len(intersections) - 1, 2):
                self.span(y, math.ceil(intersections[index] - 0.5), math.floor(intersections[index + 1] - 0.5), color)

    def circle(self, cx: float, cy: float, radius: float, color: tuple[int, int, int, int]) -> None:
        cx, cy, radius = cx * self.width, cy * self.height, radius * self.width
        first = max(0, int(math.floor(cy - radius)))
        last = min(self.height - 1, int(math.ceil(cy + radius)))
        for y in range(first, last + 1):
            dy = y + 0.5 - cy
            if abs(dy) <= radius:
                dx = math.sqrt(max(0.0, radius * radius - dy * dy))
                self.span(y, math.ceil(cx - dx - 0.5), math.floor(cx + dx - 0.5), color)

    def rounded_rect(self, color: tuple[int, int, int, int], inset: float, radius: float) -> None:
        left, top = inset * self.width, inset * self.height
        right, bottom = self.width - left, self.height - top
        r = radius * self.width
        for y in range(max(0, int(top)), min(self.height, int(bottom) + 1)):
            cy = y + 0.5
            dy = top + r - cy if cy < top + r else cy - (bottom - r) if cy > bottom - r else 0.0
            dx = math.sqrt(max(0.0, r * r - dy * dy)) if dy else r
            self.span(y, math.ceil(left + r - dx - 0.5), math.floor(right - r + dx - 0.5), color)


def draw_face(canvas: Canvas, template: bool = False) -> None:
    dark = (0, 0, 0, 255) if template else OUTLINE
    face = (0, 0, 0, 255) if template else FACE
    canvas.polygon(HEAD_OUTER, dark)
    canvas.polygon(FACE_INNER, face)
    if not template:
        canvas.polygon(LEFT_EAR, INNER_EAR)
        canvas.polygon(RIGHT_EAR, INNER_EAR)
    canvas.polygon(MASK, dark)
    if not template:
        for x, y in EYES:
            canvas.circle(x, y, 2.24 / 64, EYE)
            canvas.circle(x, y, 0.90 / 64, OUTLINE)
        canvas.polygon(NOSE_SHAPE, NOSE)
        canvas.polygon(MOUTH_CENTER, dark)
        canvas.polygon(MOUTH_LEFT, dark)
        canvas.polygon(MOUTH_RIGHT, dark)


def raster_pixels(size: int, state: str | None = None, template: bool = False) -> bytes:
    scale = 3
    canvas = Canvas(size * scale, size * scale)
    if not template:
        canvas.rounded_rect(ACCENT, 0.03, 0.24)
    draw_face(canvas, template)
    if state:
        ring = (23, 21, 31, 255)
        dot = CONNECTED if state == "connected" else DISCONNECTED
        canvas.circle(0.81, 0.81, 0.13, ring)
        canvas.circle(0.81, 0.81, 0.09, dot)
    return downsample(canvas, size, scale)


def raster_icon(size: int, state: str | None = None, template: bool = False) -> bytes:
    return png_encode(raster_pixels(size, state, template), size)


def downsample(canvas: Canvas, size: int, scale: int) -> bytes:
    result = bytearray(size * size * 4)
    samples = scale * scale
    for y in range(size):
        for x in range(size):
            sums = [0, 0, 0, 0]
            for sy in range(scale):
                for sx in range(scale):
                    source = ((y * scale + sy) * canvas.width + x * scale + sx) * 4
                    alpha = canvas.pixels[source + 3]
                    sums[3] += alpha
                    for channel in range(3):
                        sums[channel] += canvas.pixels[source + channel] * alpha // 255
            destination = (y * size + x) * 4
            alpha = sums[3] // samples
            result[destination + 3] = alpha
            if alpha:
                for channel in range(3):
                    result[destination + channel] = min(255, sums[channel] * 255 // max(1, sums[3]))
    return bytes(result)


def png_encode(rgba: bytes, size: int) -> bytes:
    raw = b"".join(b"\x00" + rgba[y * size * 4:(y + 1) * size * 4] for y in range(size))
    def chunk(kind: bytes, data: bytes) -> bytes:
        body = kind + data
        return struct.pack(">I", len(data)) + body + struct.pack(">I", zlib.crc32(body) & 0xffffffff)
    header = struct.pack(">2I5B", size, size, 8, 6, 0, 0, 0)
    return b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", header) + chunk(b"IDAT", zlib.compress(raw, 9)) + chunk(b"IEND", b"")


def make_ico(pngs: dict[int, bytes]) -> bytes:
    entries, payload = [], bytearray()
    offset = 6 + 16 * len(pngs)
    for size, png in sorted(pngs.items()):
        dimension = 0 if size >= 256 else size
        entries.append(struct.pack("<BBBBHHII", dimension, dimension, 0, 0, 1, 32, len(png), offset))
        payload.extend(png)
        offset += len(png)
    return struct.pack("<HHH", 0, 1, len(entries)) + b"".join(entries) + bytes(payload)


def make_icns(pngs: dict[int, bytes]) -> bytes:
    types = {16: b"icp4", 32: b"icp5", 64: b"icp6", 128: b"ic07", 256: b"ic08", 512: b"ic09", 1024: b"ic10"}
    chunks = [types[size] + struct.pack(">I", len(png) + 8) + png for size, png in sorted(pngs.items()) if size in types]
    payload = b"".join(chunks)
    return b"icns" + struct.pack(">I", len(payload) + 8) + payload


def generate(output: Path) -> None:
    output.mkdir(parents=True, exist_ok=True)
    (output / "racc-connect.svg").write_text(svg_document(), encoding="utf-8", newline="\n")
    (output / "racc-menubar-template.svg").write_text(svg_document(template=True), encoding="utf-8", newline="\n")
    sizes = (16, 32, 48, 64, 128, 256, 512, 1024)
    pngs = {size: raster_icon(size) for size in sizes}
    for size, data in pngs.items():
        (output / f"racc-connect-{size}.png").write_bytes(data)
    (output / "racc-connect.ico").write_bytes(make_ico({size: pngs[size] for size in (16, 32, 48, 256)}))
    (output / "racc-connect.icns").write_bytes(make_icns(pngs))
    (output / "racc-menubar-template.png").write_bytes(raster_icon(64, template=True))
    (output / "racc-tray-connected.png").write_bytes(raster_icon(64, state="connected"))
    (output / "racc-tray-disconnected.png").write_bytes(raster_icon(64, state="disconnected"))
    (output / "racc-tray-disconnected.rgba").write_bytes(raster_pixels(32, state="disconnected"))
    (output / "racc-tray-connected.rgba").write_bytes(raster_pixels(32, state="connected"))
    (output / "racc-menubar-template.rgba").write_bytes(raster_pixels(32, template=True))


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--output", type=Path, default=ROOT / "assets" / "icons")
    generate(parser.parse_args().output)


if __name__ == "__main__":
    main()
