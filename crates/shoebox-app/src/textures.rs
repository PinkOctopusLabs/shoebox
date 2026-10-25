//! GPU textures for loaded images, under an LRU memory budget.

use std::collections::HashMap;

use crate::jobs::{Key, Size};

struct Entry {
    tex: egui::TextureHandle,
    bytes: usize,
    last_used: u64,
}

pub struct Textures {
    entries: HashMap<Key, Entry>,
    /// Keys that failed to load; not requested again until a rescan.
    pub failed: HashMap<Key, String>,
    budget: usize,
    used: usize,
    frame: u64,
}

impl Textures {
    pub fn new(budget_bytes: usize) -> Self {
        Self {
            entries: HashMap::new(),
            failed: HashMap::new(),
            budget: budget_bytes,
            used: 0,
            frame: 0,
        }
    }

    /// Change the memory budget (evicting down to it on the next insert).
    pub fn set_budget(&mut self, bytes: usize) {
        self.budget = bytes;
    }

    /// Bytes of textures held now.
    pub fn used(&self) -> usize {
        self.used
    }

    /// Call once per frame before drawing.
    pub fn begin_frame(&mut self) {
        self.frame += 1;
    }

    pub fn get(&mut self, key: &Key) -> Option<egui::TextureHandle> {
        let e = self.entries.get_mut(key)?;
        e.last_used = self.frame;
        Some(e.tex.clone())
    }

    pub fn contains(&self, key: &Key) -> bool {
        self.entries.contains_key(key)
    }

    /// The best texture available for a photo, preferring `want` and larger.
    pub fn best(&mut self, photo: u64, want: Size) -> Option<egui::TextureHandle> {
        let order: &[Size] = match want {
            Size::Small => &[Size::Small, Size::Large, Size::Full],
            Size::Large => &[Size::Large, Size::Full, Size::Small],
            Size::Full => &[Size::Full, Size::Large, Size::Small],
        };
        order
            .iter()
            .find_map(|&size| self.get(&Key { photo, size }))
    }

    pub fn insert(&mut self, ctx: &egui::Context, key: Key, img: &shoebox_decode::Rgb8) {
        // Larger than the GPU takes would end the app (images are decoded
        // to fit, `jobs::MAX_TEXTURE_SIDE`; this is the backstop).
        let max = ctx.input(|i| i.max_texture_side) as u32;
        let fitted;
        let img = if img.width.max(img.height) > max {
            fitted = img.fit_long_edge(max);
            &fitted
        } else {
            img
        };
        let size = [img.width as usize, img.height as usize];
        let color = egui::ColorImage::from_rgba_unmultiplied(size, &img.to_rgba());
        let tex = ctx.load_texture(format!("{key:?}"), color, egui::TextureOptions::LINEAR);
        let bytes = size[0] * size[1] * 4;
        if let Some(old) = self.entries.insert(
            key,
            Entry {
                tex,
                bytes,
                last_used: self.frame,
            },
        ) {
            self.used -= old.bytes;
        }
        self.used += bytes;
        self.evict();
    }

    /// Drop least-recently-used textures (never ones used this frame) until
    /// under budget.
    fn evict(&mut self) {
        if self.used <= self.budget {
            return;
        }
        let mut by_age: Vec<(u64, Key)> = self
            .entries
            .iter()
            .map(|(k, e)| (e.last_used, *k))
            .collect();
        by_age.sort_unstable_by_key(|(t, _)| *t);
        for (t, k) in by_age {
            if self.used <= self.budget || t >= self.frame {
                break;
            }
            if let Some(e) = self.entries.remove(&k) {
                self.used -= e.bytes;
            }
        }
    }

    /// Forget everything about photos no longer in the catalog.
    pub fn retain_photos(&mut self, keep: impl Fn(&u64) -> bool) {
        let gone: Vec<Key> = self
            .entries
            .keys()
            .filter(|k| !keep(&k.photo))
            .copied()
            .collect();
        for k in gone {
            if let Some(e) = self.entries.remove(&k) {
                self.used -= e.bytes;
            }
        }
        self.failed.retain(|k, _| keep(&k.photo));
    }

    pub fn used_bytes(&self) -> usize {
        self.used
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn larger_than_the_gpu_takes_is_fitted() {
        let ctx = egui::Context::default();
        ctx.run_ui(egui::RawInput::default(), |_| {})
            .textures_delta
            .clear();
        let max = ctx.input(|i| i.max_texture_side) as u32;
        let img =
            shoebox_decode::Rgb8::new(max * 2 + 10, 3, vec![0; ((max * 2 + 10) * 3 * 3) as usize])
                .unwrap();
        let mut t = Textures::new(1 << 30);
        let key = Key {
            photo: 1,
            size: Size::Full,
        };
        t.insert(&ctx, key, &img);
        let [w, _] = t.get(&key).unwrap().size();
        assert_eq!(w as u32, max);
        drop(t);
        ctx.run_ui(egui::RawInput::default(), |_| {})
            .textures_delta
            .clear();
    }
}
