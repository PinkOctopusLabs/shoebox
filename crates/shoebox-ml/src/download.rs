//! Fetch a model once, from pinned Hugging Face revisions, checking each
//! file's SHA-256 before it's used. An interrupted download resumes. SigLIP's
//! float32 checkpoint is converted ([`crate::convert`]) and then removed.

use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use sha2::{Digest, Sha256};

use crate::convert::{TEXT_FILE, VISION_FILE};
use crate::{MlError, Result, io};

pub const TOKENIZER_FILE: &str = "tokenizer.json";
const CHECKPOINT: &str = "model.safetensors";
pub const YUNET_FILE: &str = "face_detection_yunet_2023mar.onnx";
pub const SFACE_FILE: &str = "face_recognition_sface_2021dec.onnx";
/// Written last: its presence (holding the model's id) means the folder is
/// complete.
const READY_FILE: &str = "READY";

/// One file to fetch.
pub struct Remote {
    repo: &'static str,
    revision: &'static str,
    name: &'static str,
    size: u64,
    sha256: &'static str,
    /// Fetched from here instead of Hugging Face (`repo` then names where
    /// it's from, for the settings window).
    direct: Option<&'static str>,
}

impl Remote {
    fn url(&self) -> String {
        match self.direct {
            Some(u) => u.to_owned(),
            None => format!(
                "https://huggingface.co/{}/resolve/{}/{}",
                self.repo, self.revision, self.name
            ),
        }
    }
}

/// A model: what to fetch, and what a complete folder holds.
pub struct Spec {
    /// Written to the READY file; a different id means "fetch again".
    pub id: &'static str,
    files: &'static [Remote],
    /// Files a complete folder holds.
    ready: &'static [&'static str],
    /// Split SigLIP's checkpoint into half-precision halves.
    convert: bool,
}

/// SigLIP 2 base (Google, Apache 2.0): semantic search and tags.
pub const SIGLIP: Spec = Spec {
    id: crate::MODEL_ID,
    files: &[
        Remote {
            repo: "google/siglip2-base-patch16-224",
            revision: "75de2d55ec2d0b4efc50b3e9ad70dba96a7b2fa2",
            name: TOKENIZER_FILE,
            size: 34_363_039,
            sha256: "cb9140fae3ac5122c972d37adf83e1248471a38147ad76f8215c8872c6fd8322",
            direct: None,
        },
        Remote {
            repo: "google/siglip2-base-patch16-224",
            revision: "75de2d55ec2d0b4efc50b3e9ad70dba96a7b2fa2",
            name: CHECKPOINT,
            size: 1_500_800_904,
            sha256: "612923381c76ec5a9bed335d1c48827e3f2e506ac31b044b63b2031fadee6a0b",
            direct: None,
        },
    ],
    ready: &[VISION_FILE, TEXT_FILE, TOKENIZER_FILE],
    convert: true,
};

/// OpenCV Zoo's YuNet (MIT) and SFace (Apache 2.0): people.
pub const FACES: Spec = Spec {
    id: crate::faces::MODEL_ID,
    files: &[
        Remote {
            repo: "opencv/face_detection_yunet",
            revision: "3cc26e7f1014a5ee5d74a42acee58bafc9d0a310",
            name: YUNET_FILE,
            size: 232_589,
            sha256: "8f2383e4dd3cfbb4553ea8718107fc0423210dc964f9f4280604804ed2552fa4",
            direct: None,
        },
        Remote {
            repo: "opencv/face_recognition_sface",
            revision: "3d7082438a6e4551e840c9b2bb60b71e8da4b524",
            name: SFACE_FILE,
            size: 38_696_353,
            sha256: "0ba9fbfa01b5270c96627c4ef784da859931e02f04419c829e83484087c34e79",
            direct: None,
        },
    ],
    ready: &[YUNET_FILE, SFACE_FILE],
    convert: false,
};

/// SAM 2.1 Tiny (Meta, Apache 2.0): masks of objects clicked on.
pub const SAM: Spec = Spec {
    id: crate::sam::MODEL_ID,
    files: &[Remote {
        repo: "facebook/sam2.1-hiera-tiny",
        revision: "de431c4043854a71d8101e17995dfe596bf101a5",
        name: crate::sam::FILE,
        size: 156_008_466,
        sha256: "7402e0d864fa82708a20fbd15bc84245c2f26dff0eb43a4b5b93452deb34be69",
        direct: None,
    }],
    ready: &[crate::sam::FILE],
    convert: false,
};

/// RawNIND's UtNet2 Bayer denoiser (Brummer and De Vleeschouwer; GPL-3.0
/// and CC BY 4.0): AI raw denoise. Only on the authors' Google Drive
/// (the 2024-02-21 Bayer checkpoint darktable-ai uses), checked by its
/// SHA-256.
pub const RAWNIND: Spec = Spec {
    id: crate::rawnind::MODEL_ID,
    files: &[Remote {
        repo: "trougnouf/rawnind_jddc",
        revision: "2024-02-21",
        name: crate::rawnind::FILE,
        size: 31_059_270,
        sha256: "c1f7de3de24cb12f4803ee65e11cfb467e9a5b80c3cb7126746ef026106c15e0",
        direct: Some(
            "https://drive.usercontent.google.com/download?id=1dFTLeljWi9wwojcZUsam8bE31JdYy3oM&export=download&confirm=t",
        ),
    }],
    ready: &[crate::rawnind::FILE],
    convert: false,
};

/// UperNet on ConvNeXt-B (ImageNet-22K) trained on ADE20K, by ConvNeXt's
/// authors (Meta, MIT): landscape masks. Only on Meta's own file server
/// (the facebookresearch/ConvNeXt semantic segmentation table), checked
/// by its SHA-256.
pub const SCENE: Spec = Spec {
    id: crate::scene::MODEL_ID,
    files: &[Remote {
        repo: "facebookresearch/ConvNeXt",
        revision: "048efcea897d999aed302f2639b6270aedf8d4c8",
        name: crate::scene::FILE,
        size: 488_619_917,
        sha256: "6f8e36a4c826ad0579f5a190e8e4429867013dd2c3ac1e5402c4a0e3fb8e3db8",
        direct: Some(
            "https://dl.fbaipublicfiles.com/convnext/ade20k/upernet_convnext_base_22k_640x640.pth",
        ),
    }],
    ready: &[crate::scene::FILE],
    convert: false,
};

/// MI-GAN 512 trained on Places2 (Picsart AI Research, MIT; weights MIT
/// by the repo's LICENSE-WEIGHTS): object removal. Only on the authors'
/// Google Drive (linked from github.com/Picsart-AI-Research/MI-GAN),
/// checked by its SHA-256.
pub const INPAINT: Spec = Spec {
    id: crate::inpaint::MODEL_ID,
    files: &[Remote {
        repo: "Picsart-AI-Research/MI-GAN",
        revision: "2b793c5ece43f4253e32d4afc257120a5deed6f5",
        name: crate::inpaint::FILE,
        size: 29_553_256,
        sha256: "1d6087eee0aac8923ad2606be5d8caeb4824d3e4de331995e420c74e124a466a",
        direct: Some(
            "https://drive.usercontent.google.com/download?id=1D_YCuCgo20S2256sqpedsmENNm2WMtVY&export=download&confirm=t",
        ),
    }],
    ready: &[crate::inpaint::FILE],
    convert: false,
};

impl Spec {
    /// Bytes to download.
    pub fn bytes(&self) -> u64 {
        self.files.iter().map(|f| f.size).sum()
    }

    /// Where it comes from, for the settings window.
    pub fn source(&self) -> String {
        if let Some((f, url)) = self.files.iter().find_map(|f| Some((f, f.direct?))) {
            let host = url.split('/').nth(2).unwrap_or_default();
            return if host.contains("google") {
                format!("github.com/{} (the authors' Google Drive)", f.repo)
            } else {
                format!("github.com/{} (the authors' {host})", f.repo)
            };
        }
        let mut repos: Vec<&str> = self.files.iter().map(|f| f.repo).collect();
        repos.dedup();
        format!("huggingface.co/{}", repos.join(" and huggingface.co/"))
    }

    /// Is a complete copy in `dir`?
    pub fn is_ready(&self, dir: &Path) -> bool {
        std::fs::read_to_string(dir.join(READY_FILE)).is_ok_and(|s| s.trim() == self.id)
            && self.ready.iter().all(|f| dir.join(f).is_file())
    }
}

/// What [`fetch`] is doing, for a progress bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    /// Bytes downloaded of [`Spec::bytes`] (including earlier attempts).
    Downloading {
        done: u64,
    },
    Verifying,
    Converting,
}

/// Download, verify (and for SigLIP convert) `spec` into `dir`. Returns at
/// once if it's already there. The download and conversion happen in
/// `staging` (a folder in the app's cache directory: SigLIP's float32
/// checkpoint is deleted there once converted); only the finished files
/// move into `dir`. `cancel` stops it (a later call resumes).
pub fn fetch(
    spec: &Spec,
    dir: &Path,
    staging: &Path,
    progress: &dyn Fn(Stage),
    cancel: &AtomicBool,
) -> Result<()> {
    if spec.is_ready(dir) {
        return Ok(());
    }
    for d in [dir, staging] {
        std::fs::create_dir_all(d).map_err(io(d))?;
    }
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_connect(Some(Duration::from_secs(30)))
        .timeout_recv_response(Some(Duration::from_secs(60)))
        .http_status_as_error(false)
        .build()
        .into();
    let converted = |d: &Path| d.join(VISION_FILE).is_file() && d.join(TEXT_FILE).is_file();
    let mut before = 0;
    for f in spec.files {
        let dst = staging.join(f.name);
        let done = if spec.convert && f.name == CHECKPOINT {
            converted(staging) || converted(dir)
        } else {
            dir.join(f.name).is_file()
        };
        if !done && !dst.is_file() {
            let part = part_path(&dst);
            get(
                &agent,
                f,
                &part,
                &|n| progress(Stage::Downloading { done: before + n }),
                cancel,
            )?;
            progress(Stage::Verifying);
            if sha256(&part)? != f.sha256 {
                // A damaged download, in the cache: start again next time.
                let _ = std::fs::remove_file(&part);
                return Err(MlError::Checksum {
                    file: f.name.into(),
                });
            }
            std::fs::rename(&part, &dst).map_err(io(&dst))?;
        }
        before += f.size;
    }
    if cancel.load(Ordering::Relaxed) {
        return Err(MlError::Cancelled);
    }
    if spec.convert {
        progress(Stage::Converting);
        let checkpoint = staging.join(CHECKPOINT);
        if checkpoint.is_file() {
            crate::convert::split_f16(&checkpoint, staging)?;
            std::fs::remove_file(&checkpoint).map_err(io(&checkpoint))?;
        }
    }
    for name in spec.ready {
        let from = staging.join(name);
        if from.is_file() {
            let to = dir.join(name);
            std::fs::rename(&from, &to).map_err(io(&to))?;
        }
    }
    let ready = dir.join(READY_FILE);
    std::fs::write(&ready, spec.id).map_err(io(&ready))?;
    Ok(())
}

fn part_path(dst: &Path) -> PathBuf {
    let mut s = dst.as_os_str().to_owned();
    s.push(".part");
    PathBuf::from(s)
}

/// Download `f` into `part`, resuming what's already there.
fn get(
    agent: &ureq::Agent,
    f: &Remote,
    part: &Path,
    progress: &dyn Fn(u64),
    cancel: &AtomicBool,
) -> Result<()> {
    let mut have = std::fs::metadata(part).map(|m| m.len()).unwrap_or(0);
    if have > f.size {
        have = 0;
    }
    if have == f.size {
        return Ok(());
    }
    let url = f.url();
    let mut req = agent.get(&url);
    if have > 0 {
        req = req.header("Range", format!("bytes={have}-"));
    }
    let resp = req.call().map_err(|e| MlError::Http(e.to_string()))?;
    let status = resp.status().as_u16();
    let mut out = match status {
        206 => OpenOptions::new()
            .append(true)
            .open(part)
            .map_err(io(part))?,
        200 => {
            have = 0;
            File::create(part).map_err(io(part))?
        }
        _ => return Err(MlError::Http(format!("{url}: HTTP {status}"))),
    };
    let mut body = resp.into_body().into_reader();
    let mut buf = vec![0u8; 1 << 20];
    let mut last = 0;
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(MlError::Cancelled);
        }
        let n = body
            .read(&mut buf)
            .map_err(|e| MlError::Http(e.to_string()))?;
        if n == 0 {
            break;
        }
        out.write_all(&buf[..n]).map_err(io(part))?;
        have += n as u64;
        if have - last >= 4 << 20 {
            last = have;
            progress(have);
        }
    }
    out.sync_all().map_err(io(part))?;
    progress(have);
    if have != f.size {
        return Err(MlError::Http(format!(
            "{}: got {have} of {} bytes",
            f.name, f.size
        )));
    }
    Ok(())
}

fn sha256(path: &Path) -> Result<String> {
    let mut f = File::open(path).map_err(io(path))?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 4 << 20];
    loop {
        let n = f.read(&mut buf).map_err(io(path))?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ready_needs_marker_and_files() -> anyhow::Result<()> {
        let dir = tempfile::tempdir()?;
        assert!(!SIGLIP.is_ready(dir.path()));
        for f in [VISION_FILE, TEXT_FILE, TOKENIZER_FILE] {
            std::fs::write(dir.path().join(f), b"x")?;
        }
        assert!(!SIGLIP.is_ready(dir.path()), "no marker yet");
        std::fs::write(dir.path().join(READY_FILE), "some-other-model")?;
        assert!(!SIGLIP.is_ready(dir.path()), "marker for another model");
        std::fs::write(dir.path().join(READY_FILE), SIGLIP.id)?;
        assert!(SIGLIP.is_ready(dir.path()));
        assert!(!FACES.is_ready(dir.path()));
        Ok(())
    }

    #[test]
    fn sha256_known_value() -> anyhow::Result<()> {
        let dir = tempfile::tempdir()?;
        let p = dir.path().join("f");
        std::fs::write(&p, b"abc")?;
        assert_eq!(
            sha256(&p)?,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        Ok(())
    }

    /// Network: resume a partial tokenizer download from Hugging Face.
    /// `cargo test -p shoebox-ml -- --ignored resumes`
    #[test]
    #[ignore]
    fn resumes_partial_download() -> anyhow::Result<()> {
        let dir = tempfile::tempdir()?;
        let f = &SIGLIP.files[0];
        let part = dir.path().join("t.part");
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .build()
            .into();
        // First 1000 bytes, then resume.
        let mut first = agent
            .get(&f.url())
            .header("Range", "bytes=0-999")
            .call()?
            .into_body()
            .into_reader();
        let mut head = Vec::new();
        first.read_to_end(&mut head)?;
        assert_eq!(head.len(), 1000, "server honours Range");
        std::fs::write(&part, &head)?;
        get(&agent, f, &part, &|_| {}, &AtomicBool::new(false))?;
        assert_eq!(sha256(&part)?, f.sha256);
        Ok(())
    }
}
