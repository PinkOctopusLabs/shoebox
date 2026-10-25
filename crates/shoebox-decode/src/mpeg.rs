//! MPEG-1 and MPEG-2 program streams (`.mpg`): just enough to find the
//! picture size and duration, from the start and end of the file. They
//! carry no creation date (the library dates them by the file's).

use std::io::{Read, Seek, SeekFrom};

use crate::DecodeError;
use crate::mov::MovInfo;

/// How much of each end of the file is looked at.
const WINDOW: u64 = 256 << 10;
/// The system clock ticks at 90 kHz.
const CLOCK: f64 = 90_000.0;

/// Read a stream's size (from its first sequence header) and duration
/// (from the first and last pack headers' clocks).
pub fn info<R: Read + Seek>(r: &mut R) -> Result<MovInfo, DecodeError> {
    let io = |e: std::io::Error| DecodeError::Format(format!("reading the movie: {e}"));
    let len = r.seek(SeekFrom::End(0)).map_err(io)?;
    let mut head = Vec::new();
    r.seek(SeekFrom::Start(0)).map_err(io)?;
    r.take(WINDOW).read_to_end(&mut head).map_err(io)?;
    if !head.starts_with(&[0, 0, 1, 0xBA]) && !head.starts_with(&[0, 0, 1, 0xB3]) {
        return Err(DecodeError::Format("not an MPEG stream".into()));
    }
    let mut tail = Vec::new();
    r.seek(SeekFrom::Start(len.saturating_sub(WINDOW)))
        .map_err(io)?;
    r.take(WINDOW).read_to_end(&mut tail).map_err(io)?;

    let (width, height) = starts(&head, 0xB3)
        .find_map(|at| {
            let b = head.get(at..at + 3)?;
            let w = u32::from(b[0]) << 4 | u32::from(b[1]) >> 4;
            let h = u32::from(b[1] & 0x0F) << 8 | u32::from(b[2]);
            (w > 0 && h > 0).then_some((w, h))
        })
        .unzip();
    let first = starts(&head, 0xBA).find_map(|at| clock(&head[at..]));
    let last = starts(&tail, 0xBA)
        .filter_map(|at| clock(&tail[at..]))
        .last();
    let duration_secs = first
        .zip(last)
        .filter(|(a, b)| b > a)
        .map(|(a, b)| (b - a) as f64 / CLOCK);
    Ok(MovInfo {
        width,
        height,
        duration_secs,
        ..MovInfo::default()
    })
}

/// Where each `00 00 01 <code>` start code's body begins.
fn starts(b: &[u8], code: u8) -> impl Iterator<Item = usize> + '_ {
    b.windows(4)
        .enumerate()
        .filter(move |(_, w)| *w == [0, 0, 1, code])
        .map(|(i, _)| i + 4)
}

/// A pack header's system clock reference (its 33-bit base, 90 kHz).
fn clock(b: &[u8]) -> Option<u64> {
    let b: [u64; 5] = std::array::from_fn(|i| b.get(i).copied().map_or(0, u64::from));
    match b[0] >> 4 {
        // MPEG-1: '0010', then 3 + 15 + 15 bits, each run ending in a marker.
        0b0010 => {
            Some((b[0] >> 1 & 7) << 30 | b[1] << 22 | (b[2] >> 1) << 15 | b[3] << 7 | b[4] >> 1)
        }
        // MPEG-2: '01', then the same 33 bits laid out with the markers moved.
        0b0100..=0b0111 => Some(
            (b[0] >> 3 & 7) << 30
                | (b[0] & 3) << 28
                | b[1] << 20
                | (b[2] >> 3) << 15
                | (b[2] & 3) << 13
                | b[3] << 5
                | b[4] >> 3,
        ),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    /// An MPEG-1 pack header with this clock.
    fn pack1(scr: u64) -> Vec<u8> {
        vec![
            0,
            0,
            1,
            0xBA,
            (0x20 | (scr >> 29 & 0x0E) | 1) as u8,
            (scr >> 22) as u8,
            ((scr >> 14 & 0xFE) | 1) as u8,
            (scr >> 7) as u8,
            ((scr << 1 & 0xFE) | 1) as u8,
        ]
    }

    #[test]
    fn size_and_duration_from_the_headers() {
        let mut s = pack1(90_000);
        // Sequence header: 640 × 480.
        s.extend([0, 0, 1, 0xB3, 0x28, 0x01, 0xE0, 0x14]);
        s.extend(vec![0xFF; 1000]);
        s.extend(pack1(90_000 + 3 * 90_000 + 45_000));
        s.extend(vec![0xFF; 100]);
        let i = info(&mut std::io::Cursor::new(s)).unwrap();
        assert_eq!((i.width, i.height), (Some(640), Some(480)));
        assert_eq!(i.duration_secs, Some(3.5));
        assert_eq!(i.created_utc, None);
    }

    #[test]
    fn mpeg2_clock() {
        // base = 0x1_2345_6789: '01', bits 32..30, marker, bits 29..28, ...
        let scr: u64 = 0x1_2345_6789;
        let b = [
            (0x44 | (scr >> 27 & 0x38) | (scr >> 28 & 3)) as u8,
            (scr >> 20) as u8,
            ((scr >> 12 & 0xF8) | 4 | (scr >> 13 & 3)) as u8,
            (scr >> 5) as u8,
            ((scr << 3 & 0xF8) | 4) as u8,
        ];
        assert_eq!(clock(&b), Some(scr));
    }

    #[test]
    fn not_mpeg() {
        assert!(info(&mut std::io::Cursor::new(b"RIFF....".to_vec())).is_err());
    }
}
