//! Split the downloaded float32 checkpoint into half-precision
//! `vision.safetensors` and `text.safetensors`. Streams one tensor at a time,
//! so it needs little memory, and writes each output to a `.part` file that
//! is renamed into place when complete.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::{MlError, Result, io};

pub const VISION_FILE: &str = "vision.safetensors";
pub const TEXT_FILE: &str = "text.safetensors";

#[derive(Debug, Clone, Deserialize, Serialize)]
struct Info {
    dtype: String,
    shape: Vec<usize>,
    data_offsets: [usize; 2],
}

/// Which output a tensor belongs to (None: dropped).
fn part_of(name: &str) -> Option<&'static str> {
    if name.starts_with("vision_model.") {
        Some(VISION_FILE)
    } else if name.starts_with("text_model.") || name == "logit_scale" || name == "logit_bias" {
        Some(TEXT_FILE)
    } else {
        None
    }
}

fn read_header(r: &mut impl Read, path: &Path) -> Result<(u64, BTreeMap<String, Info>)> {
    let mut len = [0u8; 8];
    r.read_exact(&mut len).map_err(io(path))?;
    let len = u64::from_le_bytes(len);
    if len > 100 << 20 {
        return Err(MlError::Format(format!(
            "{}: header too large",
            path.display()
        )));
    }
    let mut buf = vec![0u8; len as usize];
    r.read_exact(&mut buf).map_err(io(path))?;
    let mut map: BTreeMap<String, serde_json::Value> = serde_json::from_slice(&buf)
        .map_err(|e| MlError::Format(format!("{}: {e}", path.display())))?;
    map.remove("__metadata__");
    let infos = map
        .into_iter()
        .map(|(k, v)| {
            serde_json::from_value::<Info>(v)
                .map(|i| (k, i))
                .map_err(|e| MlError::Format(format!("{}: {e}", path.display())))
        })
        .collect::<Result<_>>()?;
    Ok((8 + len, infos))
}

/// Convert `src` (float32 safetensors) into the two half-precision files in
/// `dir`.
pub fn split_f16(src: &Path, dir: &Path) -> Result<()> {
    let f = File::open(src).map_err(io(src))?;
    let mut r = BufReader::with_capacity(1 << 20, f);
    let (base, infos) = read_header(&mut r, src)?;
    for out in [VISION_FILE, TEXT_FILE] {
        let mut chosen: Vec<(&String, &Info)> = infos
            .iter()
            .filter(|(k, _)| part_of(k) == Some(out))
            .collect();
        if chosen.is_empty() {
            return Err(MlError::Format(format!(
                "{}: no tensors for {out}",
                src.display()
            )));
        }
        chosen.sort_by_key(|(_, i)| i.data_offsets[0]);
        // Header first: every tensor halves in size.
        let mut header = BTreeMap::new();
        let mut at = 0usize;
        for (name, i) in &chosen {
            if i.dtype != "F32" {
                return Err(MlError::Format(format!(
                    "{name}: expected F32, found {}",
                    i.dtype
                )));
            }
            let n: usize = i.shape.iter().product();
            if i.data_offsets[1] - i.data_offsets[0] != n * 4 {
                return Err(MlError::Format(format!(
                    "{name}: size doesn't match its shape"
                )));
            }
            header.insert(
                (*name).clone(),
                Info {
                    dtype: "F16".into(),
                    shape: i.shape.clone(),
                    data_offsets: [at, at + n * 2],
                },
            );
            at += n * 2;
        }
        let mut json = serde_json::to_vec(&header).map_err(|e| MlError::Format(e.to_string()))?;
        while json.len() % 8 != 0 {
            json.push(b' ');
        }
        let dst = dir.join(out);
        let part = dir.join(format!("{out}.part"));
        let mut w = BufWriter::with_capacity(1 << 20, File::create(&part).map_err(io(&part))?);
        w.write_all(&(json.len() as u64).to_le_bytes())
            .and_then(|()| w.write_all(&json))
            .map_err(io(&part))?;
        let mut buf = Vec::new();
        let mut half = Vec::new();
        for (_, i) in &chosen {
            r.seek(SeekFrom::Start(base + i.data_offsets[0] as u64))
                .map_err(io(src))?;
            buf.resize(i.data_offsets[1] - i.data_offsets[0], 0);
            r.read_exact(&mut buf).map_err(io(src))?;
            half.clear();
            half.extend(
                buf.as_chunks::<4>()
                    .0
                    .iter()
                    .flat_map(|&c| half::f16::from_f32(f32::from_le_bytes(c)).to_le_bytes()),
            );
            w.write_all(&half).map_err(io(&part))?;
        }
        w.into_inner()
            .map_err(|e| MlError::Io {
                path: part.clone(),
                source: e.into_error(),
            })?
            .sync_all()
            .map_err(io(&part))?;
        std::fs::rename(&part, &dst).map_err(io(&dst))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use candle_core::{DType, Device, Tensor};
    use std::collections::HashMap;

    #[test]
    fn splits_and_halves() -> anyhow::Result<()> {
        let dir = tempfile::tempdir()?;
        let dev = Device::Cpu;
        let mut t = HashMap::new();
        t.insert(
            "vision_model.a".to_owned(),
            Tensor::new(&[[1.0f32, -2.5], [0.125, 3.0]], &dev)?,
        );
        t.insert(
            "text_model.b".to_owned(),
            Tensor::new(&[0.5f32, 1.0, 2.0], &dev)?,
        );
        t.insert("logit_scale".to_owned(), Tensor::new(&[4.0f32], &dev)?);
        t.insert("other".to_owned(), Tensor::new(&[9.0f32], &dev)?);
        let src = dir.path().join("model.safetensors");
        candle_core::safetensors::save(&t, &src)?;
        split_f16(&src, dir.path())?;

        let v = candle_core::safetensors::load(dir.path().join(VISION_FILE), &dev)?;
        assert_eq!(v.len(), 1);
        assert_eq!(v["vision_model.a"].dtype(), DType::F16);
        assert_eq!(
            v["vision_model.a"].to_dtype(DType::F32)?.to_vec2::<f32>()?,
            vec![vec![1.0, -2.5], vec![0.125, 3.0]]
        );
        let txt = candle_core::safetensors::load(dir.path().join(TEXT_FILE), &dev)?;
        let mut names: Vec<_> = txt.keys().cloned().collect();
        names.sort();
        assert_eq!(names, ["logit_scale", "text_model.b"]);
        assert_eq!(
            txt["text_model.b"].to_dtype(DType::F32)?.to_vec1::<f32>()?,
            vec![0.5, 1.0, 2.0]
        );
        assert!(!dir.path().join(format!("{VISION_FILE}.part")).exists());
        Ok(())
    }
}
