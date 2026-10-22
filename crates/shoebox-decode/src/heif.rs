//! The HEIF container (ISO/IEC 23008-12) that AVIF files use: the primary
//! image's size and rotation from its item properties (`ispe`, `irot`), read
//! from the boxes alone. Pixels come from ImageIO.

use crate::DecodeError;

/// A box's type and contents.
struct Atom<'a> {
    kind: [u8; 4],
    body: &'a [u8],
}

/// The boxes laid end to end in `data`.
fn boxes(mut data: &[u8]) -> Result<Vec<Atom<'_>>, DecodeError> {
    let mut out = Vec::new();
    while data.len() >= 8 {
        let size = u32::from_be_bytes([data[0], data[1], data[2], data[3]]) as u64;
        let kind = [data[4], data[5], data[6], data[7]];
        let (header, size) = match size {
            0 => (8, data.len() as u64),
            1 => {
                let b = data.get(8..16).ok_or(DecodeError::Truncated)?;
                let mut a = [0u8; 8];
                a.copy_from_slice(b);
                (16, u64::from_be_bytes(a))
            }
            s => (8, s),
        };
        let size = usize::try_from(size).map_err(|_| DecodeError::Truncated)?;
        if size < header {
            return Err(DecodeError::Truncated);
        }
        if size > data.len() {
            // Cut short (the start of a file, read for its metadata): the
            // boxes before it still count.
            break;
        }
        out.push(Atom {
            kind,
            body: &data[header..size],
        });
        data = &data[size..];
    }
    Ok(out)
}

fn find<'a>(boxes: &[Atom<'a>], kind: &[u8; 4]) -> Option<&'a [u8]> {
    boxes.iter().find(|b| &b.kind == kind).map(|b| b.body)
}

fn be16(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from(u16::from_be_bytes([
        *b.get(at)?,
        *b.get(at + 1)?,
    ])))
}

fn be32(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes([
        *b.get(at)?,
        *b.get(at + 1)?,
        *b.get(at + 2)?,
        *b.get(at + 3)?,
    ]))
}

/// The primary image's stored size (`ispe`) and its rotation (`irot`) in
/// quarter turns anticlockwise, which readers apply.
pub fn primary(data: &[u8]) -> Result<((u32, u32), u8), DecodeError> {
    let fmt = |m: &str| DecodeError::Format(format!("HEIF: {m}"));
    let top = boxes(data)?;
    // `meta` is a full box: version and flags first.
    let meta = find(&top, b"meta").ok_or_else(|| fmt("no meta box"))?;
    let meta = boxes(meta.get(4..).ok_or(DecodeError::Truncated)?)?;
    let pitm = find(&meta, b"pitm").ok_or_else(|| fmt("no primary item"))?;
    let primary = if pitm.first() == Some(&0) {
        be16(pitm, 4)
    } else {
        be32(pitm, 4)
    }
    .ok_or(DecodeError::Truncated)?;
    let iprp = boxes(find(&meta, b"iprp").ok_or_else(|| fmt("no item properties"))?)?;
    let ipco = boxes(find(&iprp, b"ipco").ok_or_else(|| fmt("no property container"))?)?;
    // Which properties (1-based indexes into ipco) the primary item has.
    let mut props = Vec::new();
    for ipma in iprp.iter().filter(|b| &b.kind == b"ipma") {
        let b = ipma.body;
        let (version, wide) = (
            b.first().copied().unwrap_or(0),
            b.get(3).is_some_and(|f| f & 1 == 1),
        );
        let n = be32(b, 4).ok_or(DecodeError::Truncated)?;
        let mut at = 8;
        for _ in 0..n {
            let (item, len) = if version < 1 {
                (be16(b, at), 2)
            } else {
                (be32(b, at), 4)
            };
            let item = item.ok_or(DecodeError::Truncated)?;
            at += len;
            let count = usize::from(*b.get(at).ok_or(DecodeError::Truncated)?);
            at += 1;
            for _ in 0..count {
                let index = if wide {
                    let v = be16(b, at).ok_or(DecodeError::Truncated)?;
                    at += 2;
                    v & 0x7FFF
                } else {
                    let v = u32::from(*b.get(at).ok_or(DecodeError::Truncated)?);
                    at += 1;
                    v & 0x7F
                };
                if item == primary && index > 0 {
                    props.push(index as usize - 1);
                }
            }
        }
    }
    let mut size = None;
    let mut turns = 0;
    for p in props.into_iter().filter_map(|i| ipco.get(i)) {
        match &p.kind {
            // A full box: version and flags, then width and height.
            b"ispe" => {
                size = Some((
                    be32(p.body, 4).ok_or(DecodeError::Truncated)?,
                    be32(p.body, 8).ok_or(DecodeError::Truncated)?,
                ));
            }
            b"irot" => turns = p.body.first().map_or(0, |a| a & 3),
            _ => {}
        }
    }
    let size = size
        .filter(|&(w, h)| w > 0 && h > 0)
        .ok_or_else(|| fmt("the primary image has no size"))?;
    Ok((size, turns))
}

/// The primary image's size as shown: `ispe` with `irot` applied.
pub fn dimensions(data: &[u8]) -> Result<(u32, u32), DecodeError> {
    let ((w, h), turns) = primary(data)?;
    Ok(if turns % 2 == 1 { (h, w) } else { (w, h) })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    fn bx(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut v = ((body.len() + 8) as u32).to_be_bytes().to_vec();
        v.extend(kind);
        v.extend(body);
        v
    }

    /// A HEIF's boxes, without image data: item 1 is primary, with
    /// properties ispe (index 1) and irot (index 2) when `turns` is given.
    fn container(w: u32, h: u32, turns: Option<u8>) -> Vec<u8> {
        let mut ispe = vec![0, 0, 0, 0];
        ispe.extend(w.to_be_bytes());
        ispe.extend(h.to_be_bytes());
        let mut ipco = bx(b"ispe", &ispe);
        let mut assoc = vec![0x81];
        if let Some(t) = turns {
            ipco.extend(bx(b"irot", &[t]));
            assoc.push(0x82);
        }
        // Version 0, one entry: item 1 (u16), its associations (u8).
        let mut ipma = vec![0, 0, 0, 0, 0, 0, 0, 1, 0, 1, assoc.len() as u8];
        ipma.extend(assoc);
        let mut iprp = bx(b"ipco", &ipco);
        iprp.extend(bx(b"ipma", &ipma));
        let mut meta = vec![0, 0, 0, 0];
        meta.extend(bx(b"pitm", &[0, 0, 0, 0, 0, 1]));
        meta.extend(bx(b"iprp", &iprp));
        let mut out = bx(b"ftyp", b"avif\0\0\0\0avifmif1miaf");
        out.extend(bx(b"meta", &meta));
        out
    }

    #[test]
    fn size_and_rotation() {
        assert_eq!(dimensions(&container(64, 48, None)).unwrap(), (64, 48));
        assert_eq!(dimensions(&container(64, 48, Some(1))).unwrap(), (48, 64));
        assert_eq!(primary(&container(64, 48, Some(2))).unwrap(), ((64, 48), 2));
        assert!(dimensions(b"\0\0\0\x08ftyp").is_err());
        assert!(dimensions(b"\0\0\0\x20ftyp").is_err());
    }
}
