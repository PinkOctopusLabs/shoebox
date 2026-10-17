//! QuickTime / MP4 movies: just enough of the box structure to find the
//! display size, duration and creation date, reading only box headers and
//! the `moov` box (which may be at the end of a large file).

use std::io::{Read, Seek, SeekFrom};

use crate::DecodeError;

/// What a movie's header says.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MovInfo {
    /// Display size of the video track (after its rotation).
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub duration_secs: Option<f64>,
    /// `mvhd` creation time: seconds since 1970, UTC.
    pub created_utc: Option<i64>,
    /// Apple's `com.apple.quicktime.creationdate`: local time with its
    /// offset, e.g. "2026-04-24T13:36:30+0700" (iPhone and Live Photos).
    pub created_local: Option<String>,
}

/// Seconds between 1904-01-01 (QuickTime's epoch) and 1970-01-01.
const EPOCH_1904: i64 = 2_082_844_800;
/// Don't read a `moov` box bigger than this (it's normally a few hundred KB).
const MAX_MOOV: u64 = 64 << 20;

fn be32(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

fn be64(b: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_be_bytes(b.get(at..at + 8)?.try_into().ok()?))
}

/// Boxes in `b`: (type, body).
fn boxes(b: &[u8]) -> impl Iterator<Item = (&[u8], &[u8])> {
    let mut at = 0usize;
    std::iter::from_fn(move || {
        let size = be32(b, at)? as usize;
        let kind = b.get(at + 4..at + 8)?;
        let (header, size) = match size {
            1 => (16, usize::try_from(be64(b, at + 8)?).ok()?),
            0 => (8, b.len() - at),
            s => (8, s),
        };
        if size < header {
            return None;
        }
        let body = b.get(at + header..at + size)?;
        at += size;
        Some((kind, body))
    })
}

fn child<'a>(b: &'a [u8], kind: &[u8]) -> Option<&'a [u8]> {
    boxes(b).find(|(k, _)| *k == kind).map(|(_, body)| body)
}

/// Read a movie's header from a file, without reading the media data.
pub fn info<R: Read + Seek>(r: &mut R) -> Result<MovInfo, DecodeError> {
    let len = r
        .seek(SeekFrom::End(0))
        .map_err(|_| DecodeError::Truncated)?;
    let mut at = 0u64;
    while at + 8 <= len {
        r.seek(SeekFrom::Start(at))
            .map_err(|_| DecodeError::Truncated)?;
        let mut h = [0u8; 16];
        r.read_exact(&mut h[..8])
            .map_err(|_| DecodeError::Truncated)?;
        let mut size = u64::from(u32::from_be_bytes([h[0], h[1], h[2], h[3]]));
        let mut header = 8;
        if size == 1 {
            r.read_exact(&mut h[8..])
                .map_err(|_| DecodeError::Truncated)?;
            size = u64::from_be_bytes([h[8], h[9], h[10], h[11], h[12], h[13], h[14], h[15]]);
            header = 16;
        } else if size == 0 {
            size = len - at;
        }
        if size < header {
            break;
        }
        if &h[4..8] == b"moov" {
            let body = size - header;
            if body > MAX_MOOV {
                return Err(DecodeError::Format("movie header too large".into()));
            }
            let mut moov = vec![0u8; body as usize];
            r.read_exact(&mut moov)
                .map_err(|_| DecodeError::Truncated)?;
            return Ok(parse_moov(&moov));
        }
        at += size;
    }
    Err(DecodeError::Format("no movie header (moov)".into()))
}

fn parse_moov(moov: &[u8]) -> MovInfo {
    let mut out = MovInfo::default();
    if let Some(mvhd) = child(moov, b"mvhd") {
        // Version 1 has 64-bit times.
        let (created, scale, duration) = match mvhd.first() {
            Some(1) => (be64(mvhd, 4), be32(mvhd, 20), be64(mvhd, 24)),
            _ => (
                be32(mvhd, 4).map(u64::from),
                be32(mvhd, 12),
                be32(mvhd, 16).map(u64::from),
            ),
        };
        out.created_utc = created
            .filter(|&c| c > 0)
            .and_then(|c| i64::try_from(c).ok())
            .map(|c| c - EPOCH_1904);
        if let (Some(s), Some(d)) = (scale, duration)
            && s > 0
        {
            out.duration_secs = Some(d as f64 / f64::from(s));
        }
    }
    // The first video track's display size (tkhd), turned by its matrix.
    for (kind, trak) in boxes(moov) {
        if kind != b"trak" {
            continue;
        }
        let is_video = child(trak, b"mdia")
            .and_then(|m| child(m, b"hdlr"))
            .and_then(|h| h.get(8..12))
            == Some(b"vide".as_slice());
        let Some(tkhd) = child(trak, b"tkhd") else {
            continue;
        };
        if !is_video {
            continue;
        }
        let v1 = tkhd.first() == Some(&1);
        let base = if v1 { 4 + 32 } else { 4 + 20 };
        // Matrix (9 × 32-bit) after reserved/layer/group/volume fields.
        let matrix = base + 16;
        let (w, h) = (
            be32(tkhd, matrix + 36).map(|v| v >> 16),
            be32(tkhd, matrix + 40).map(|v| v >> 16),
        );
        // A 90° rotation has a = 0 (the first matrix entry).
        let rotated = be32(tkhd, matrix) == Some(0);
        let (w, h) = if rotated { (h, w) } else { (w, h) };
        out.width = w.filter(|&v| v > 0);
        out.height = h.filter(|&v| v > 0);
        break;
    }
    out.created_local = apple_creation_date(moov);
    out
}

/// `com.apple.quicktime.creationdate` from moov/meta/keys + ilst.
fn apple_creation_date(moov: &[u8]) -> Option<String> {
    let meta = child(moov, b"meta")?;
    let keys = child(meta, b"keys")?;
    let ilst = child(meta, b"ilst")?;
    // keys: version/flags, count, then (size, namespace, name) entries.
    let count = be32(keys, 4)? as usize;
    let mut at = 8;
    let mut index = None;
    for i in 1..=count {
        let size = be32(keys, at)? as usize;
        let name = keys.get(at + 8..at + size)?;
        if name == b"com.apple.quicktime.creationdate" {
            index = Some(i as u32);
            break;
        }
        at += size.max(8);
    }
    let index = index?;
    // ilst entries are boxes whose type is the key's 1-based index.
    let (_, item) = boxes(ilst).find(|(k, _)| be32(k, 0) == Some(index))?;
    let data = child(item, b"data")?;
    // data: type (4), locale (4), then the value.
    let value = std::str::from_utf8(data.get(8..)?).ok()?;
    Some(value.trim_end_matches('\0').to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bx(kind: &[u8], body: &[u8]) -> Vec<u8> {
        let mut v = ((body.len() + 8) as u32).to_be_bytes().to_vec();
        v.extend_from_slice(kind);
        v.extend_from_slice(body);
        v
    }

    /// A small movie: ftyp, a big mdat, then moov (as cameras write it).
    fn movie(rotate: bool) -> Vec<u8> {
        let mut mvhd = vec![0u8; 100];
        let created = (EPOCH_1904 + 1_777_000_000) as u32;
        mvhd[4..8].copy_from_slice(&created.to_be_bytes());
        mvhd[12..16].copy_from_slice(&600u32.to_be_bytes()); // timescale
        mvhd[16..20].copy_from_slice(&1800u32.to_be_bytes()); // 3 s
        let mut tkhd = vec![0u8; 84];
        let m = 4 + 20 + 16;
        let one = 0x0001_0000u32.to_be_bytes();
        if rotate {
            tkhd[m + 4..m + 8].copy_from_slice(&one); // b = 1, a = 0
        } else {
            tkhd[m..m + 4].copy_from_slice(&one);
        }
        tkhd[m + 36..m + 40].copy_from_slice(&(1920u32 << 16).to_be_bytes());
        tkhd[m + 40..m + 44].copy_from_slice(&(1080u32 << 16).to_be_bytes());
        let mut hdlr = vec![0u8; 24];
        hdlr[8..12].copy_from_slice(b"vide");
        let mdia = bx(b"mdia", &bx(b"hdlr", &hdlr));
        let trak = bx(b"trak", &[bx(b"tkhd", &tkhd), mdia].concat());
        // Apple metadata: one key, its value in ilst item 1.
        let name = b"com.apple.quicktime.creationdate";
        let mut keys = vec![0, 0, 0, 0];
        keys.extend_from_slice(&1u32.to_be_bytes());
        keys.extend_from_slice(&((name.len() + 8) as u32).to_be_bytes());
        keys.extend_from_slice(b"mdta");
        keys.extend_from_slice(name);
        let mut data = vec![0, 0, 0, 1, 0, 0, 0, 0];
        data.extend_from_slice(b"2026-04-24T13:36:30+0700");
        let item = {
            let body = bx(b"data", &data);
            let mut v = ((body.len() + 8) as u32).to_be_bytes().to_vec();
            v.extend_from_slice(&1u32.to_be_bytes());
            v.extend_from_slice(&body);
            v
        };
        let meta = bx(b"meta", &[bx(b"keys", &keys), bx(b"ilst", &item)].concat());
        let moov = bx(b"moov", &[bx(b"mvhd", &mvhd), trak, meta].concat());
        [bx(b"ftyp", b"qt  "), bx(b"mdat", &vec![7u8; 5000]), moov].concat()
    }

    #[test]
    fn reads_size_duration_and_dates() {
        let i = info(&mut std::io::Cursor::new(movie(false))).unwrap();
        assert_eq!((i.width, i.height), (Some(1920), Some(1080)));
        assert_eq!(i.duration_secs, Some(3.0));
        assert_eq!(i.created_utc, Some(1_777_000_000));
        assert_eq!(i.created_local.as_deref(), Some("2026-04-24T13:36:30+0700"));
        let r = info(&mut std::io::Cursor::new(movie(true))).unwrap();
        assert_eq!((r.width, r.height), (Some(1080), Some(1920)), "portrait");
        assert!(info(&mut std::io::Cursor::new(b"not a movie".to_vec())).is_err());
    }
}
