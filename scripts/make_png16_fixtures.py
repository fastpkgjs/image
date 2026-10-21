import binascii
import struct
import zlib
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent / "fixtures"
ROOT.mkdir(exist_ok=True)


def chunk(kind, payload):
    return (
        struct.pack(">I", len(payload))
        + kind
        + payload
        + struct.pack(">I", binascii.crc32(kind + payload) & 0xFFFFFFFF)
    )


def write_png(name, width, height, color_type, rows):
    raw = bytearray()
    for row in rows:
        raw.append(0)
        for value in row:
            raw.extend(struct.pack(">H", value))
    data = (
        b"\x89PNG\r\n\x1a\n"
        + chunk(
            b"IHDR",
            struct.pack(">IIBBBBB", width, height, 16, color_type, 0, 0, 0),
        )
        + chunk(b"IDAT", zlib.compress(bytes(raw), 9))
        + chunk(b"IEND", b"")
    )
    (ROOT / name).write_bytes(data)


write_png(
    "png16-rgb.png",
    3,
    2,
    2,
    [
        [0, 1000, 2000, 3000, 4000, 5000, 6000, 7000, 8000],
        [65535, 32768, 1, 12345, 23456, 34567, 40000, 50000, 60000],
    ],
)
write_png(
    "png16-rgba.png",
    3,
    2,
    6,
    [
        [1000, 2000, 3000, 65535, 4000, 5000, 6000, 32768, 7000, 8000, 9000, 0],
        [
            10000,
            11000,
            12000,
            12345,
            13000,
            14000,
            15000,
            65534,
            16000,
            17000,
            18000,
            1,
        ],
    ],
)
write_png(
    "png16-gray.png",
    4,
    2,
    0,
    [[0, 1, 257, 514], [1000, 32768, 65534, 65535]],
)
write_png(
    "png16-gray-alpha.png",
    3,
    2,
    4,
    [
        [1000, 65535, 2000, 32768, 3000, 0],
        [4000, 12345, 5000, 65534, 6000, 1],
    ],
)
