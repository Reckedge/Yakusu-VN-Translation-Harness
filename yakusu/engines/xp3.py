"""Kirikiri .xp3 archives and scrambled .ks scripts.

The writer is a port of arcusmaximus's Xp3Pack (MIT): it writes an
unencrypted archive whose file-table hashes are all zero. Paired with
KirikiriUnencryptedArchive's version.dll, games with encrypted archives
still accept it, so we never need to know a game's encryption to patch it.

The reader handles plain (unencrypted) archives; encrypted ones go through
msg-tool, which knows the keys for ~500 games.
"""
import io
import os
import struct
import zlib

MAGIC = b'XP3\r\n \n\x1a\x8b\x67\x01'
FILE, INFO, SEGM, ADLR = b'File', b'info', b'segm', b'adlr'


# ----------------------------------------------------------------- writer

def _chunk(tag, payload):
    return tag + struct.pack('<Q', len(payload)) + payload


def write_archive(src_dir, out_path, files=None):
    """Pack src_dir into out_path. File names are stored relative, with '/'.

    Kirikiri resolves patch files by name only (no folders), which is what
    every translation patch relies on, but we keep relative paths as given.
    """
    if files is None:
        files = []
        for d, _, fs in os.walk(src_dir):
            for f in fs:
                files.append(os.path.relpath(os.path.join(d, f), src_dir))
        files.sort()
    index = io.BytesIO()
    with open(out_path, 'wb') as out:
        out.write(MAGIC)
        out.write(struct.pack('<Q', 0))  # index offset, patched below
        for rel in files:
            with open(os.path.join(src_dir, rel), 'rb') as f:
                data = f.read()
            compressed = not rel.lower().endswith(('.mpg', '.ogg', '.png', '.jpg', '.webp', '.mp4', '.wmv'))
            body = zlib.compress(data, 9) if compressed else data
            offset = out.tell()
            out.write(body)
            name = rel.replace('\\', '/').encode('utf-16-le')
            info = struct.pack('<IQQH', 0, len(data), len(body), len(rel)) + name
            segm = struct.pack('<IQQQ', 1 if compressed else 0, offset, len(data), len(body))
            adlr = struct.pack('<I', 0)  # zero hash marks "don't decrypt" for the version.dll
            index.write(_chunk(FILE, _chunk(INFO, info) + _chunk(SEGM, segm) + _chunk(ADLR, adlr)))
        index_offset = out.tell()
        raw = index.getvalue()
        out.write(b'\x00' + struct.pack('<Q', len(raw)) + raw)  # flag 0 = uncompressed index
        out.seek(len(MAGIC))
        out.write(struct.pack('<Q', index_offset))
    return files


# ----------------------------------------------------------------- reader

class Entry:
    def __init__(self, name, size, segments, encrypted_flag, adler):
        self.name = name
        self.size = size
        self.segments = segments  # (compressed, offset, orig, packed)
        self.encrypted_flag = encrypted_flag
        self.adler = adler


def read_index(path):
    """Return a list of Entry for an xp3 archive (works for most games)."""
    with open(path, 'rb') as f:
        head = f.read(len(MAGIC))
        if head != MAGIC:
            raise ValueError('not an xp3 archive')
        (offset,) = struct.unpack('<Q', f.read(8))
        f.seek(offset)
        flag = f.read(1)[0]
        if flag == 0x80:  # continuation header (Kirikiri 2.28+)
            f.read(8)
            (offset,) = struct.unpack('<Q', f.read(8))
            f.seek(offset)
            flag = f.read(1)[0]
        if flag & 1:
            packed, orig = struct.unpack('<QQ', f.read(16))
            raw = zlib.decompress(f.read(packed))
        else:
            (size,) = struct.unpack('<Q', f.read(8))
            raw = f.read(size)
    entries = []
    pos = 0
    while pos + 12 <= len(raw):
        tag = raw[pos:pos + 4]
        (size,) = struct.unpack_from('<Q', raw, pos + 4)
        body = raw[pos + 12:pos + 12 + size]
        pos += 12 + size
        if tag != FILE:
            continue
        name, fsize, segs, eflag, adler = None, 0, [], 0, 0
        p = 0
        while p + 12 <= len(body):
            t = body[p:p + 4]
            (s,) = struct.unpack_from('<Q', body, p + 4)
            b = body[p + 12:p + 12 + s]
            p += 12 + s
            if t == INFO:
                eflag, fsize, _packed, nlen = struct.unpack_from('<IQQH', b, 0)
                name = b[22:22 + nlen * 2].decode('utf-16-le', 'replace')
            elif t == SEGM:
                for k in range(0, len(b) - 27, 28):
                    segs.append(struct.unpack_from('<IQQQ', b, k))
            elif t == ADLR:
                (adler,) = struct.unpack_from('<I', b, 0)
        if name is not None:
            entries.append(Entry(name, fsize, segs, eflag, adler))
    return entries


def read_file(path, entry):
    out = bytearray()
    with open(path, 'rb') as f:
        for comp, off, orig, packed in entry.segments:
            f.seek(off)
            data = f.read(packed)
            out += zlib.decompress(data) if comp & 7 else data
    return bytes(out)


# ----------------------------------------------------------------- .ks helpers

def descramble(data):
    """Undo Kirikiri script scrambling (FE FE 0x/FF FE). Port of KirikiriDescrambler (MIT).

    Returns decoded text, or None if the data isn't scrambled.
    """
    if len(data) < 5 or data[0] != 0xFE or data[1] != 0xFE or data[3:5] != b'\xff\xfe':
        return None
    mode = data[2]
    body = bytearray(data[5:])
    if mode == 0:
        for i in range(0, len(body) - 1, 2):
            if body[i + 1] == 0 and body[i] < 0x20:
                continue
            body[i + 1] ^= body[i] & 0xFE
            body[i] ^= 1
    elif mode == 1:
        for i in range(0, len(body) - 1, 2):
            c = body[i] | (body[i + 1] << 8)
            c = ((c & 0xAAAA) >> 1) | ((c & 0x5555) << 1)
            body[i], body[i + 1] = c & 0xFF, c >> 8
    elif mode == 2:
        packed, orig = struct.unpack_from('<qq', body, 0)
        body = bytearray(zlib.decompress(bytes(body[16:16 + packed])))
    else:
        raise ValueError(f'unsupported scramble mode {mode}')
    return bytes(body).decode('utf-16-le', 'replace')


def looks_like_text(data):
    """Rough check that decrypted script bytes are text (used to spot encryption)."""
    if not data:
        return True
    if data[:2] in (b'\xff\xfe', b'\xfe\xff') or data[:3] == b'\xef\xbb\xbf' or data[:2] == b'\xfe\xfe':
        return True
    sample = data[:4096]
    for enc in ('utf-8', 'cp932', 'utf-16-le'):
        try:
            s = sample.decode(enc)
        except UnicodeDecodeError:
            continue
        bad = sum(1 for c in s if ord(c) < 32 and c not in '\r\n\t')
        if bad < max(3, len(s) // 50):
            return True
    return False
