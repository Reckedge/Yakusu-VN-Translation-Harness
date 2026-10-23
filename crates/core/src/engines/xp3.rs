//! Kirikiri `.xp3` archives and scrambled `.ks` scripts.
//!
//! The writer is a port of arcusmaximus's Xp3Pack (MIT): an unencrypted
//! archive whose file-table hashes are all zero. Together with
//! KirikiriUnencryptedArchive's version.dll, games with encrypted archives
//! still accept it, so a game's encryption never has to be known to patch it.
//! The reader handles plain archives; encrypted ones go through msg-tool,
//! which knows the keys for ~500 games.

use flate2::read::ZlibDecoder;
use flate2::write::ZlibEncoder;
use flate2::Compression;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

pub const MAGIC: &[u8] = b"XP3\r\n \n\x1a\x8b\x67\x01";
const FILE: &[u8] = b"File";
const INFO: &[u8] = b"info";
const SEGM: &[u8] = b"segm";
const ADLR: &[u8] = b"adlr";

fn chunk(tag: &[u8], payload: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(12 + payload.len());
    v.extend_from_slice(tag);
    v.extend_from_slice(&(payload.len() as u64).to_le_bytes());
    v.extend_from_slice(payload);
    v
}

fn zlib(data: &[u8]) -> std::io::Result<Vec<u8>> {
    let mut e = ZlibEncoder::new(Vec::new(), Compression::best());
    e.write_all(data)?;
    e.finish()
}

/// Pack `files` (paths relative to `src_dir`, '/'-separated) into `out_path`.
pub fn write_archive(src_dir: &Path, out_path: &Path, files: &[String]) -> std::io::Result<()> {
    let mut out = File::create(out_path)?;
    out.write_all(MAGIC)?;
    out.write_all(&0u64.to_le_bytes())?; // index offset, patched below
    let mut index = Vec::new();
    for rel in files {
        let data = std::fs::read(src_dir.join(rel))?;
        let lower = rel.to_ascii_lowercase();
        let compressed = ![".mpg", ".ogg", ".png", ".jpg", ".webp", ".mp4", ".wmv"].iter().any(|e| lower.ends_with(e));
        let body = if compressed { zlib(&data)? } else { data.clone() };
        let offset = out.stream_position()?;
        out.write_all(&body)?;
        let name: Vec<u16> = rel.replace('\\', "/").encode_utf16().collect();
        let mut info = Vec::new();
        info.extend_from_slice(&0u32.to_le_bytes());
        info.extend_from_slice(&(data.len() as u64).to_le_bytes());
        info.extend_from_slice(&(body.len() as u64).to_le_bytes());
        info.extend_from_slice(&(name.len() as u16).to_le_bytes());
        for u in &name {
            info.extend_from_slice(&u.to_le_bytes());
        }
        let mut segm = Vec::new();
        segm.extend_from_slice(&(compressed as u32).to_le_bytes());
        segm.extend_from_slice(&offset.to_le_bytes());
        segm.extend_from_slice(&(data.len() as u64).to_le_bytes());
        segm.extend_from_slice(&(body.len() as u64).to_le_bytes());
        // Zero hash: tells the version.dll not to decrypt this file.
        let adlr = 0u32.to_le_bytes();
        let mut file_chunk = chunk(INFO, &info);
        file_chunk.extend(chunk(SEGM, &segm));
        file_chunk.extend(chunk(ADLR, &adlr));
        index.extend(chunk(FILE, &file_chunk));
    }
    let index_offset = out.stream_position()?;
    out.write_all(&[0u8])?; // flag 0: uncompressed index
    out.write_all(&(index.len() as u64).to_le_bytes())?;
    out.write_all(&index)?;
    out.seek(SeekFrom::Start(MAGIC.len() as u64))?;
    out.write_all(&index_offset.to_le_bytes())?;
    Ok(())
}

#[derive(Clone, Debug)]
pub struct Entry {
    pub name: String,
    pub size: u64,
    /// (flags, offset, original size, packed size)
    pub segments: Vec<(u32, u64, u64, u64)>,
    pub encrypted_flag: u32,
    pub adler: u32,
}

fn rd_u32(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}
fn rd_u64(b: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(b[at..at + 8].try_into().unwrap())
}

/// The file table of an xp3 archive (works for most unencrypted indexes).
pub fn read_index(path: &Path) -> std::io::Result<Vec<Entry>> {
    let bad = || std::io::Error::new(std::io::ErrorKind::InvalidData, "not a readable xp3 index");
    let mut f = File::open(path)?;
    let mut head = vec![0u8; MAGIC.len()];
    f.read_exact(&mut head)?;
    if head != MAGIC {
        return Err(bad());
    }
    let mut b8 = [0u8; 8];
    f.read_exact(&mut b8)?;
    let mut offset = u64::from_le_bytes(b8);
    f.seek(SeekFrom::Start(offset))?;
    let mut flag = [0u8; 1];
    f.read_exact(&mut flag)?;
    if flag[0] == 0x80 {
        // Continuation header (Kirikiri 2.28+).
        f.read_exact(&mut b8)?;
        f.read_exact(&mut b8)?;
        offset = u64::from_le_bytes(b8);
        f.seek(SeekFrom::Start(offset))?;
        f.read_exact(&mut flag)?;
    }
    let raw = if flag[0] & 1 == 1 {
        f.read_exact(&mut b8)?;
        let packed = u64::from_le_bytes(b8);
        f.read_exact(&mut b8)?;
        if packed > 512 * 1024 * 1024 {
            return Err(bad());
        }
        let mut buf = vec![0u8; packed as usize];
        f.read_exact(&mut buf)?;
        let mut out = Vec::new();
        ZlibDecoder::new(&buf[..]).read_to_end(&mut out).map_err(|_| bad())?;
        out
    } else {
        f.read_exact(&mut b8)?;
        let size = u64::from_le_bytes(b8);
        if size > 512 * 1024 * 1024 {
            return Err(bad());
        }
        let mut buf = vec![0u8; size as usize];
        f.read_exact(&mut buf)?;
        buf
    };
    let mut entries = Vec::new();
    let mut pos = 0usize;
    while pos + 12 <= raw.len() {
        let tag = &raw[pos..pos + 4];
        let size = rd_u64(&raw, pos + 4) as usize;
        let end = (pos + 12).saturating_add(size).min(raw.len());
        let body = &raw[pos + 12..end];
        pos = end;
        if tag != FILE {
            continue;
        }
        let (mut name, mut fsize, mut segs, mut eflag, mut adler) = (None, 0u64, Vec::new(), 0u32, 0u32);
        let mut p = 0usize;
        while p + 12 <= body.len() {
            let t = &body[p..p + 4];
            let s = rd_u64(body, p + 4) as usize;
            let e = (p + 12).saturating_add(s).min(body.len());
            let b = &body[p + 12..e];
            p = e;
            if t == INFO && b.len() >= 22 {
                eflag = rd_u32(b, 0);
                fsize = rd_u64(b, 4);
                let nlen = u16::from_le_bytes([b[20], b[21]]) as usize;
                let units: Vec<u16> = b[22..(22 + nlen * 2).min(b.len())]
                    .chunks_exact(2)
                    .map(|c| u16::from_le_bytes([c[0], c[1]]))
                    .collect();
                name = Some(String::from_utf16_lossy(&units));
            } else if t == SEGM {
                let mut k = 0;
                while k + 28 <= b.len() {
                    segs.push((rd_u32(b, k), rd_u64(b, k + 4), rd_u64(b, k + 12), rd_u64(b, k + 20)));
                    k += 28;
                }
            } else if t == ADLR && b.len() >= 4 {
                adler = rd_u32(b, 0);
            }
        }
        if let Some(name) = name {
            entries.push(Entry { name, size: fsize, segments: segs, encrypted_flag: eflag, adler });
        }
    }
    Ok(entries)
}

pub fn read_file(path: &Path, entry: &Entry) -> std::io::Result<Vec<u8>> {
    let mut f = File::open(path)?;
    let mut out = Vec::with_capacity(entry.size as usize);
    for &(flags, off, _orig, packed) in &entry.segments {
        f.seek(SeekFrom::Start(off))?;
        let mut buf = vec![0u8; packed as usize];
        f.read_exact(&mut buf)?;
        if flags & 7 != 0 {
            ZlibDecoder::new(&buf[..]).read_to_end(&mut out)?;
        } else {
            out.extend_from_slice(&buf);
        }
    }
    Ok(out)
}

/// Undo Kirikiri script scrambling (`FE FE 0x FF FE`). Port of
/// KirikiriDescrambler (MIT). `None` if the data isn't scrambled.
pub fn descramble(data: &[u8]) -> Option<String> {
    if data.len() < 5 || data[0] != 0xFE || data[1] != 0xFE || data[3] != 0xFF || data[4] != 0xFE {
        return None;
    }
    let mode = data[2];
    let mut body = data[5..].to_vec();
    match mode {
        0 => {
            let mut i = 0;
            while i + 1 < body.len() {
                if !(body[i + 1] == 0 && body[i] < 0x20) {
                    body[i + 1] ^= body[i] & 0xFE;
                    body[i] ^= 1;
                }
                i += 2;
            }
        }
        1 => {
            let mut i = 0;
            while i + 1 < body.len() {
                let c = u16::from_le_bytes([body[i], body[i + 1]]);
                let c = ((c & 0xAAAA) >> 1) | ((c & 0x5555) << 1);
                body[i..i + 2].copy_from_slice(&c.to_le_bytes());
                i += 2;
            }
        }
        2 => {
            if body.len() < 16 {
                return None;
            }
            let packed = i64::from_le_bytes(body[0..8].try_into().ok()?) as usize;
            let mut out = Vec::new();
            ZlibDecoder::new(&body[16..(16 + packed).min(body.len())]).read_to_end(&mut out).ok()?;
            body = out;
        }
        _ => return None,
    }
    let (t, _, _) = encoding_rs::UTF_16LE.decode(&body);
    Some(t.into_owned())
}

/// Rough check that script bytes are text (used to spot encryption).
pub fn looks_like_text(data: &[u8]) -> bool {
    if data.is_empty() {
        return true;
    }
    if data.starts_with(&[0xFF, 0xFE])
        || data.starts_with(&[0xFE, 0xFF])
        || data.starts_with(&[0xEF, 0xBB, 0xBF])
        || data.starts_with(&[0xFE, 0xFE])
    {
        return true;
    }
    let sample = &data[..data.len().min(4096)];
    let ok = |s: &str| {
        let bad = s.chars().filter(|c| (*c as u32) < 32 && !"\r\n\t".contains(*c)).count();
        bad < std::cmp::max(3, s.chars().count() / 50)
    };
    if let Ok(s) = std::str::from_utf8(sample) {
        if ok(s) {
            return true;
        }
    }
    if let Some(s) = encoding_rs::SHIFT_JIS.decode_without_bom_handling_and_without_replacement(sample) {
        if ok(&s) {
            return true;
        }
    }
    if sample.len().is_multiple_of(2) {
        if let Some(s) = encoding_rs::UTF_16LE.decode_without_bom_handling_and_without_replacement(sample) {
            if ok(&s) {
                return true;
            }
        }
    }
    false
}
