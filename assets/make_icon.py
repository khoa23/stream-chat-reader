"""Vẽ assets/icon.ico (bong bóng chat có sóng âm). Chạy: python assets/make_icon.py"""
import struct
import zlib
from pathlib import Path

SIZES = [16, 24, 32, 48, 256]
SAMPLES = 4
BACKGROUND = (37, 99, 235)
BUBBLE = (255, 255, 255)
# Thanh sóng âm: tâm x, nửa chiều cao
BARS = [(0.34, 0.08), (0.50, 0.15), (0.66, 0.08)]
BAR_CENTER_Y = 0.43
BAR_HALF_WIDTH = 0.045


def in_rounded_rect(x, y, left, top, right, bottom, radius):
    nearest_x = min(max(x, left + radius), right - radius)
    nearest_y = min(max(y, top + radius), bottom - radius)
    return (x - nearest_x) ** 2 + (y - nearest_y) ** 2 <= radius**2


def in_tail(x, y):
    # Tam giác đuôi bong bóng, đỉnh dưới ở (0.28, 0.86)
    return 0.28 <= x <= 0.48 and 0.64 <= y <= 0.86 - (x - 0.28) * 1.1


def color_at(x, y):
    """Trả về màu (r, g, b) tại một điểm trong hình vuông đơn vị, None nếu trong suốt."""
    if not in_rounded_rect(x, y, 0.03, 0.03, 0.97, 0.97, 0.2):
        return None
    if in_rounded_rect(x, y, 0.15, 0.18, 0.85, 0.68, 0.12) or in_tail(x, y):
        for bar_x, half_height in BARS:
            if in_rounded_rect(
                x, y,
                bar_x - BAR_HALF_WIDTH, BAR_CENTER_Y - half_height,
                bar_x + BAR_HALF_WIDTH, BAR_CENTER_Y + half_height,
                BAR_HALF_WIDTH,
            ):
                return BACKGROUND
        return BUBBLE
    return BACKGROUND


def render(size):
    """Trả về các dòng điểm ảnh RGBA (từ trên xuống) với khử răng cưa."""
    rows = []
    for py in range(size):
        row = []
        for px in range(size):
            red = green = blue = alpha = 0
            for sy in range(SAMPLES):
                for sx in range(SAMPLES):
                    color = color_at((px + (sx + 0.5) / SAMPLES) / size, (py + (sy + 0.5) / SAMPLES) / size)
                    if color:
                        red += color[0]
                        green += color[1]
                        blue += color[2]
                        alpha += 1
            if alpha:
                row.append((red // alpha, green // alpha, blue // alpha, 255 * alpha // SAMPLES**2))
            else:
                row.append((0, 0, 0, 0))
        rows.append(row)
    return rows


def png_bytes(rows):
    def chunk(kind, data):
        body = kind + data
        return struct.pack(">I", len(data)) + body + struct.pack(">I", zlib.crc32(body))

    size = len(rows)
    raw = b"".join(b"\0" + b"".join(bytes(pixel) for pixel in row) for row in rows)
    return (
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", struct.pack(">IIBBBBB", size, size, 8, 6, 0, 0, 0))
        + chunk(b"IDAT", zlib.compress(raw, 9))
        + chunk(b"IEND", b"")
    )


def dib_bytes(rows):
    """Ảnh BGRA dưới dạng DIB của ICO: dòng từ dưới lên, kèm mặt nạ AND."""
    size = len(rows)
    mask_stride = ((size + 31) // 32) * 4
    header = struct.pack("<IiiHHIIiiII", 40, size, size * 2, 1, 32, 0, 0, 0, 0, 0, 0)
    pixels = b"".join(
        bytes((b, g, r, a)) for row in reversed(rows) for r, g, b, a in row
    )
    return header + pixels + bytes(mask_stride * size)


def main():
    images = []
    for size in SIZES:
        rows = render(size)
        # Ảnh 256 dùng PNG cho nhẹ; ảnh nhỏ dùng DIB để mọi API của Windows đều đọc được
        images.append((size, png_bytes(rows) if size == 256 else dib_bytes(rows)))

    offset = 6 + 16 * len(images)
    directory = struct.pack("<HHH", 0, 1, len(images))
    for size, data in images:
        side = 0 if size == 256 else size
        directory += struct.pack("<BBBBHHII", side, side, 0, 0, 1, 32, len(data), offset)
        offset += len(data)
    path = Path(__file__).with_name("icon.ico")
    path.write_bytes(directory + b"".join(data for _, data in images))
    print(f"{path}: {path.stat().st_size} bytes")


if __name__ == "__main__":
    main()
