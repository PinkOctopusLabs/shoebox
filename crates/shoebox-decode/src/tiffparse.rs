//! A minimal, bounds-checked TIFF IFD walker.
//!
//! Enough to find NEF embedded previews and TIFF strip/tile data without
//! decoding pixels. Classic TIFF only (NEF and Lightroom exports are not BigTIFF).

use crate::DecodeError;

pub const TAG_NEW_SUBFILE_TYPE: u16 = 0x00FE;
pub const TAG_IMAGE_WIDTH: u16 = 0x0100;
pub const TAG_IMAGE_LENGTH: u16 = 0x0101;
pub const TAG_COMPRESSION: u16 = 0x0103;
pub const TAG_STRIP_OFFSETS: u16 = 0x0111;
pub const TAG_ORIENTATION: u16 = 0x0112;
pub const TAG_STRIP_BYTE_COUNTS: u16 = 0x0117;
pub const TAG_TILE_OFFSETS: u16 = 0x0144;
pub const TAG_TILE_BYTE_COUNTS: u16 = 0x0145;
pub const TAG_SUB_IFDS: u16 = 0x014A;
pub const TAG_JPEG_IF_OFFSET: u16 = 0x0201;
pub const TAG_JPEG_IF_LENGTH: u16 = 0x0202;

/// Upper bound on entries per IFD and IFDs visited, so corrupt files can't loop.
const MAX_ENTRIES: u16 = 4096;
const MAX_IFDS: usize = 64;
/// Upper bound on values read from one entry (strip/tile tables, SubIFD lists).
/// 1M strips is far beyond any real file and caps allocation at 8 MB.
const MAX_VALUES: usize = 1 << 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Endian {
    Little,
    Big,
}

#[derive(Debug, Clone)]
pub struct Entry {
    pub tag: u16,
    pub typ: u16,
    pub count: u32,
    /// Offset of the value bytes within the file (inline or pointed-to).
    value_at: usize,
}

#[derive(Debug, Clone, Default)]
pub struct Ifd {
    pub entries: Vec<Entry>,
}

impl Ifd {
    pub fn get(&self, tag: u16) -> Option<&Entry> {
        self.entries.iter().find(|e| e.tag == tag)
    }
}

/// A parsed TIFF container borrowing the file bytes.
pub struct Tiff<'a> {
    data: &'a [u8],
    endian: Endian,
    first_ifd: usize,
}

fn type_size(typ: u16) -> Option<usize> {
    Some(match typ {
        1 | 2 | 6 | 7 => 1,   // BYTE, ASCII, SBYTE, UNDEFINED
        3 | 8 => 2,           // SHORT, SSHORT
        4 | 9 | 11 | 13 => 4, // LONG, SLONG, FLOAT, IFD
        5 | 10 | 12 => 8,     // RATIONAL, SRATIONAL, DOUBLE
        _ => return None,
    })
}

impl<'a> Tiff<'a> {
    pub fn parse(data: &'a [u8]) -> Result<Self, DecodeError> {
        let endian = match data.get(0..4) {
            Some(b"II*\0") => Endian::Little,
            Some(b"MM\0*") => Endian::Big,
            _ => return Err(DecodeError::Format("not a TIFF container".into())),
        };
        let mut t = Tiff {
            data,
            endian,
            first_ifd: 0,
        };
        t.first_ifd = t.u32_at(4)? as usize;
        Ok(t)
    }

    fn bytes(&self, at: usize, len: usize) -> Result<&'a [u8], DecodeError> {
        at.checked_add(len)
            .and_then(|end| self.data.get(at..end))
            .ok_or(DecodeError::Truncated)
    }

    fn u16_at(&self, at: usize) -> Result<u16, DecodeError> {
        let b = self.bytes(at, 2)?;
        let a = [b[0], b[1]];
        Ok(match self.endian {
            Endian::Little => u16::from_le_bytes(a),
            Endian::Big => u16::from_be_bytes(a),
        })
    }

    fn u32_at(&self, at: usize) -> Result<u32, DecodeError> {
        let b = self.bytes(at, 4)?;
        let a = [b[0], b[1], b[2], b[3]];
        Ok(match self.endian {
            Endian::Little => u32::from_le_bytes(a),
            Endian::Big => u32::from_be_bytes(a),
        })
    }

    pub fn read_ifd(&self, at: usize) -> Result<(Ifd, usize), DecodeError> {
        let n = self.u16_at(at)?;
        if n > MAX_ENTRIES {
            return Err(DecodeError::Format("implausible IFD entry count".into()));
        }
        let mut entries = Vec::with_capacity(usize::from(n));
        for i in 0..usize::from(n) {
            let e = at + 2 + i * 12;
            let tag = self.u16_at(e)?;
            let typ = self.u16_at(e + 2)?;
            let count = self.u32_at(e + 4)?;
            let Some(size) = type_size(typ) else {
                continue; // unknown type: skip, as the spec allows
            };
            let total = size
                .checked_mul(count as usize)
                .ok_or(DecodeError::Truncated)?;
            let value_at = if total <= 4 {
                e + 8
            } else {
                self.u32_at(e + 8)? as usize
            };
            entries.push(Entry {
                tag,
                typ,
                count,
                value_at,
            });
        }
        let next = self.u32_at(at + 2 + usize::from(n) * 12)? as usize;
        Ok((Ifd { entries }, next))
    }

    /// Integer values of an entry (BYTE/SHORT/LONG/IFD types).
    pub fn uints(&self, e: &Entry) -> Result<Vec<u64>, DecodeError> {
        let size = type_size(e.typ).ok_or(DecodeError::Truncated)?;
        let count = e.count as usize;
        if count > MAX_VALUES {
            return Err(DecodeError::Format(format!(
                "tag {:#x} has an implausible {count} values",
                e.tag
            )));
        }
        // Guard against values past the end of the file before allocating.
        self.bytes(e.value_at, size.saturating_mul(count))?;
        (0..count)
            .map(|i| {
                let at = e.value_at + i * size;
                Ok(match e.typ {
                    1 | 7 => u64::from(self.bytes(at, 1)?[0]),
                    3 => u64::from(self.u16_at(at)?),
                    4 | 13 => u64::from(self.u32_at(at)?),
                    _ => {
                        return Err(DecodeError::Format(format!(
                            "tag {:#x} is not an integer",
                            e.tag
                        )));
                    }
                })
            })
            .collect()
    }

    pub fn uint(&self, ifd: &Ifd, tag: u16) -> Result<Option<u64>, DecodeError> {
        match ifd.get(tag) {
            Some(e) => Ok(self.uints(e)?.first().copied()),
            None => Ok(None),
        }
    }

    pub fn ifd0(&self) -> Result<Ifd, DecodeError> {
        Ok(self.read_ifd(self.first_ifd)?.0)
    }

    /// IFD0, its chain, and every SubIFD (depth-first), capped at `MAX_IFDS`.
    pub fn all_ifds(&self) -> Result<Vec<Ifd>, DecodeError> {
        let mut out = Vec::new();
        let mut stack = vec![self.first_ifd];
        let mut seen = std::collections::HashSet::new();
        while let Some(at) = stack.pop() {
            if out.len() >= MAX_IFDS {
                break;
            }
            if at == 0 || !seen.insert(at) {
                continue;
            }
            let (ifd, next) = self.read_ifd(at)?;
            if let Some(e) = ifd.get(TAG_SUB_IFDS) {
                // Only the first few SubIFDs can ever be visited.
                stack.extend(
                    self.uints(e)?
                        .into_iter()
                        .take(MAX_IFDS)
                        .map(|o| o as usize),
                );
            }
            stack.push(next);
            out.push(ifd);
        }
        Ok(out)
    }

    /// Byte ranges for paired offset/count tags (strips or tiles).
    pub fn chunk_ranges(
        &self,
        ifd: &Ifd,
        offsets_tag: u16,
        counts_tag: u16,
    ) -> Result<Option<Vec<std::ops::Range<usize>>>, DecodeError> {
        let (Some(o), Some(c)) = (ifd.get(offsets_tag), ifd.get(counts_tag)) else {
            return Ok(None);
        };
        let offs = self.uints(o)?;
        let counts = self.uints(c)?;
        if offs.len() != counts.len() {
            return Err(DecodeError::Format(
                "strip/tile offset and count mismatch".into(),
            ));
        }
        let mut ranges = Vec::with_capacity(offs.len());
        for (&off, &len) in offs.iter().zip(&counts) {
            let (off, len) = (off as usize, len as usize);
            self.bytes(off, len)?;
            ranges.push(off..off + len);
        }
        Ok(Some(ranges))
    }

    pub fn data(&self) -> &'a [u8] {
        self.data
    }
}
