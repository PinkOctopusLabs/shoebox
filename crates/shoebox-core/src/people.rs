//! People: faces found in photos, grouped by likeness, and the names you give
//! the groups.
//!
//! Faces (from the app's face models) are stored per file in the embeddings
//! database ([`encode`] / [`decode`]): rebuildable. Grouping ([`group`]) is
//! recomputed from them. Names are your decisions, so they live in
//! `people.json` in the data directory (like settings), each with example
//! faces: after any regrouping or cache rebuild, a group goes to the named
//! person its faces resemble ([`assign`]). Faces you said aren't someone are
//! remembered too, by file and position.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::embeddings::{Payload, Vector};
use crate::error::{CoreError, Result, io};

/// One face in a photo.
#[derive(Debug, Clone, PartialEq)]
pub struct Face {
    /// x, y, width, height as fractions of the upright image.
    pub rect: [f32; 4],
    /// Detector confidence (0–1).
    pub score: f32,
    pub vec: Vector,
}

/// Bytes per stored face: rect, score, scale (f32 each), then the vector.
fn face_len(dim: usize) -> usize {
    6 * 4 + dim
}

/// A file's faces as a stored row (an empty list means "no faces").
pub fn encode(faces: &[Face]) -> Payload {
    let dim = faces.first().map_or(0, |f| f.vec.q.len());
    let mut data = Vec::with_capacity(faces.len() * face_len(dim));
    for f in faces {
        for v in f.rect.iter().chain([&f.score, &f.vec.scale]) {
            data.extend(v.to_le_bytes());
        }
        data.extend(f.vec.q.iter().map(|&x| x as u8));
    }
    // `scale` holds the vector length, so rows decode without knowing it.
    Payload {
        scale: dim as f32,
        data,
    }
}

pub fn decode(p: &Payload) -> Vec<Face> {
    let dim = p.scale.max(0.0) as usize;
    if dim == 0 {
        return Vec::new();
    }
    p.data
        .chunks_exact(face_len(dim))
        .map(|c| {
            let f =
                |i: usize| f32::from_le_bytes([c[i * 4], c[i * 4 + 1], c[i * 4 + 2], c[i * 4 + 3]]);
            Face {
                rect: [f(0), f(1), f(2), f(3)],
                score: f(4),
                vec: Vector {
                    scale: f(5),
                    q: c[24..].iter().map(|&x| x as i8).collect(),
                },
            }
        })
        .collect()
}

/// A face in the library: which file, which face in it.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, PartialOrd, Ord)]
pub struct FaceRef {
    pub path: PathBuf,
    pub index: usize,
}

/// Group faces by likeness: each face joins the group whose average face it
/// resembles most, if that's at least `min` (cosine), else starts a group;
/// then every face is placed once more against the final averages. Faces
/// are taken in `order` (best detected first, so groups start from clear
/// faces). Returns a group number per face.
pub fn group(faces: &[&Vector], order: &[usize], min: f32) -> Vec<usize> {
    let dim = faces.first().map_or(0, |v| v.q.len());
    let as_f32 = |v: &Vector| -> Vec<f32> { v.q.iter().map(|&x| f32::from(x) * v.scale).collect() };
    let unit = |mut v: Vec<f32>| {
        let n = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        if n > 0.0 {
            v.iter_mut().for_each(|x| *x /= n);
        }
        v
    };
    let dot = |a: &[f32], b: &[f32]| a.iter().zip(b).map(|(x, y)| x * y).sum::<f32>();
    let vecs: Vec<Vec<f32>> = faces.iter().map(|v| unit(as_f32(v))).collect();
    let mut sums: Vec<Vec<f32>> = Vec::new();
    let mut centres: Vec<Vec<f32>> = Vec::new();
    let mut of = vec![usize::MAX; faces.len()];
    let best = |centres: &[Vec<f32>], v: &[f32]| {
        centres
            .iter()
            .enumerate()
            .map(|(g, c)| (g, dot(c, v)))
            .max_by(|a, b| a.1.total_cmp(&b.1))
    };
    for &i in order {
        let v = &vecs[i];
        let g = match best(&centres, v) {
            Some((g, s)) if s >= min => g,
            _ => {
                sums.push(vec![0.0; dim]);
                centres.push(vec![0.0; dim]);
                sums.len() - 1
            }
        };
        sums[g].iter_mut().zip(v).for_each(|(s, x)| *s += x);
        centres[g] = unit(sums[g].clone());
        of[i] = g;
    }
    // Second pass against the settled averages.
    for (i, v) in vecs.iter().enumerate() {
        if let Some((g, s)) = best(&centres, v)
            && s >= min
        {
            of[i] = g;
        }
    }
    // Renumber densely, biggest group first.
    let mut sizes: HashMap<usize, usize> = HashMap::new();
    for &g in &of {
        *sizes.entry(g).or_default() += 1;
    }
    let mut ranked: Vec<(usize, usize)> = sizes.into_iter().collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    let renum: HashMap<usize, usize> = ranked
        .iter()
        .enumerate()
        .map(|(n, (g, _))| (*g, n))
        .collect();
    of.iter().map(|g| renum[g]).collect()
}

/// A person you named.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Person {
    pub id: String,
    pub name: String,
    /// Example faces (quantised vectors) a group is compared with.
    #[serde(default)]
    pub examples: Vec<StoredVector>,
    /// Faces you said aren't this person.
    #[serde(default)]
    pub not: Vec<FaceRef>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StoredVector {
    pub scale: f32,
    pub q: Vec<i8>,
}

impl From<&Vector> for StoredVector {
    fn from(v: &Vector) -> Self {
        Self {
            scale: v.scale,
            q: v.q.clone(),
        }
    }
}

impl From<&StoredVector> for Vector {
    fn from(v: &StoredVector) -> Self {
        Vector {
            scale: v.scale,
            q: v.q.clone(),
        }
    }
}

/// Example faces kept per person (shared evenly between the groups merged
/// into them, so each look of a person is kept).
pub const MAX_EXAMPLES: usize = 48;
/// A face is compared with this many of a person's closest examples.
const CLOSEST: usize = 3;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct People {
    #[serde(default)]
    pub people: Vec<Person>,
}

impl People {
    pub fn load(path: &Path) -> Result<Self> {
        match std::fs::read(path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map_err(|e| CoreError::Invalid(format!("{}: {e}", path.display()))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(io(path)(e)),
        }
    }

    /// Atomic write in the app's data directory (never the library).
    pub fn save(&self, path: &Path) -> Result<()> {
        let dir = path.parent().unwrap_or(Path::new("."));
        std::fs::create_dir_all(dir).map_err(io(dir))?;
        let tmp = path.with_extension("json.tmp");
        let json =
            serde_json::to_vec(self).map_err(|e| CoreError::Invalid(format!("people: {e}")))?;
        std::fs::write(&tmp, json).map_err(io(&tmp))?;
        std::fs::rename(&tmp, path).map_err(io(path))?;
        Ok(())
    }

    /// Name a group: a new person with examples from its faces, or more
    /// examples for an existing person with that name.
    pub fn name(&mut self, name: &str, faces: &[&Vector]) -> String {
        self.merge(name, &[], &[faces.to_vec()])
    }

    /// Make one person, called `name`, of these named people and unnamed
    /// groups (their faces): someone already called that (any case), else
    /// the first of `people` (renamed), else someone new. The others' example
    /// faces and "not them" corrections move to that person, each source
    /// getting an even share of the examples; the others are removed.
    /// Returns the person's id.
    pub fn merge(&mut self, name: &str, people: &[String], groups: &[Vec<&Vector>]) -> String {
        let name = name.trim();
        let target = self
            .people
            .iter()
            .find(|p| p.name.eq_ignore_ascii_case(name))
            .or_else(|| {
                people
                    .iter()
                    .find_map(|id| self.people.iter().find(|p| &p.id == id))
            })
            .map(|p| p.id.clone());
        let id = target.unwrap_or_else(|| {
            let id = crate::events::new_id().replacen("ev-", "person-", 1);
            self.people.push(Person {
                id: id.clone(),
                name: String::new(),
                examples: Vec::new(),
                not: Vec::new(),
            });
            id
        });
        // The others, taken out.
        let (gone, kept): (Vec<Person>, Vec<Person>) = std::mem::take(&mut self.people)
            .into_iter()
            .partition(|p| p.id != id && people.contains(&p.id));
        self.people = kept;
        let Some(person) = self.people.iter_mut().find(|p| p.id == id) else {
            return id;
        };
        person.name = name.to_owned();
        let mut sources: Vec<Vec<Vector>> =
            vec![person.examples.iter().map(Vector::from).collect()];
        for p in gone {
            sources.push(p.examples.iter().map(Vector::from).collect());
            for f in p.not {
                if !person.not.contains(&f) {
                    person.not.push(f);
                }
            }
        }
        sources.extend(
            groups
                .iter()
                .map(|g| g.iter().map(|v| (*v).clone()).collect()),
        );
        sources.retain(|s| !s.is_empty());
        let share = (MAX_EXAMPLES / sources.len().max(1)).max(1);
        person.examples = sources
            .iter()
            .flat_map(|s| spread(&s.iter().collect::<Vec<_>>(), share))
            .collect();
        id
    }
}

/// Up to `n` of these vectors, evenly through the list.
fn spread(v: &[&Vector], n: usize) -> Vec<StoredVector> {
    let k = v.len().min(n);
    (0..k)
        .map(|i| StoredVector::from(v[i * v.len() / k]))
        .collect()
}

/// Which named person each group is, if any: the person the group's faces
/// resemble most on average, if that's at least `min`. A face is compared
/// with the person's few closest examples, not all of them, so a person
/// with several looks (ages, glasses, angles) matches each of them.
/// `groups[g]` lists group g's face vectors.
pub fn assign(groups: &[Vec<&Vector>], people: &People, min: f32) -> Vec<Option<usize>> {
    let examples: Vec<Vec<Vector>> = people
        .people
        .iter()
        .map(|p| p.examples.iter().map(Vector::from).collect())
        .collect();
    groups
        .iter()
        .map(|faces| {
            if faces.is_empty() {
                return None;
            }
            examples
                .iter()
                .enumerate()
                .filter(|(_, ex)| !ex.is_empty())
                .map(|(p, ex)| {
                    let total: f32 = faces
                        .iter()
                        .map(|f| {
                            let mut sims: Vec<f32> = ex.iter().map(|e| e.dot(f)).collect();
                            sims.sort_by(|a, b| b.total_cmp(a));
                            let k = sims.len().min(CLOSEST);
                            sims[..k].iter().sum::<f32>() / k as f32
                        })
                        .sum();
                    (p, total / faces.len() as f32)
                })
                .filter(|&(_, s)| s >= min)
                .max_by(|a, b| a.1.total_cmp(&b.1))
                .map(|(p, _)| p)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unit(v: &[f32]) -> Vector {
        let n = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        Vector::quantise(&v.iter().map(|x| x / n).collect::<Vec<_>>())
    }

    #[test]
    fn faces_round_trip() {
        let faces = vec![
            Face {
                rect: [0.1, 0.2, 0.3, 0.4],
                score: 0.9,
                vec: unit(&[1.0, 2.0, 3.0]),
            },
            Face {
                rect: [0.5, 0.5, 0.1, 0.1],
                score: 0.8,
                vec: unit(&[-1.0, 0.5, 0.0]),
            },
        ];
        assert_eq!(decode(&encode(&faces)), faces);
        assert!(decode(&encode(&[])).is_empty());
    }

    #[test]
    fn groups_alike_faces() {
        // Two people (around x and around y), plus one face like neither.
        let a = [
            unit(&[1.0, 0.1, 0.0]),
            unit(&[0.9, 0.0, 0.1]),
            unit(&[1.0, -0.1, 0.05]),
        ];
        let b = [unit(&[0.0, 1.0, 0.1]), unit(&[0.1, 0.9, 0.0])];
        let c = unit(&[0.0, 0.0, 1.0]);
        let all: Vec<&Vector> = a.iter().chain(&b).chain([&c]).collect();
        let order: Vec<usize> = (0..all.len()).collect();
        let g = group(&all, &order, 0.8);
        assert_eq!(g[0], g[1]);
        assert_eq!(g[0], g[2]);
        assert_eq!(g[3], g[4]);
        assert_ne!(g[0], g[3]);
        assert_ne!(g[5], g[0]);
        assert_ne!(g[5], g[3]);
        assert_eq!(g[0], 0, "biggest group first");
    }

    #[test]
    fn names_follow_groups() -> anyhow::Result<()> {
        let a = [unit(&[1.0, 0.1, 0.0]), unit(&[0.9, 0.0, 0.1])];
        let b = [unit(&[0.0, 1.0, 0.1])];
        let mut people = People::default();
        people.name("Ann", &a.iter().collect::<Vec<_>>());
        // After regrouping, a group of new faces like Ann's is Ann; others aren't.
        let new_a = [unit(&[1.0, 0.0, 0.05])];
        let groups = vec![new_a.iter().collect::<Vec<_>>(), b.iter().collect()];
        assert_eq!(assign(&groups, &people, 0.7), [Some(0), None]);
        // Naming another group "ann" adds examples to the same person.
        let id = people.name("ann", &new_a.iter().collect::<Vec<_>>());
        assert_eq!(people.people.len(), 1);
        assert_eq!(id, people.people[0].id);
        assert_eq!(people.people[0].examples.len(), 3);

        let dir = tempfile::tempdir()?;
        let path = dir.path().join("people.json");
        let people = people;
        assert_eq!(People::load(&path)?, People::default());
        people.save(&path)?;
        assert_eq!(People::load(&path)?, people);
        Ok(())
    }

    /// Four groups of one person (two looks), marked as the same person:
    /// one person with examples from each, so new faces of either look are
    /// recognised.
    #[test]
    fn merging_groups_keeps_every_look() {
        let young: Vec<Vector> = (0..30)
            .map(|i| unit(&[1.0, 0.02 * i as f32, 0.0, 0.0]))
            .collect();
        let older: Vec<Vector> = (0..30)
            .map(|i| unit(&[0.6, 0.0, 0.8, 0.01 * i as f32]))
            .collect();
        let bob = [unit(&[0.0, 0.0, 0.0, 1.0])];
        let mut people = People::default();
        people.name("Bob", &bob.iter().collect::<Vec<_>>());
        let first = people.name("Ann", &young[..15].iter().collect::<Vec<_>>());
        // Another named group, and two unnamed ones, are all Ann.
        let second = people.name("Annie", &older[..15].iter().collect::<Vec<_>>());
        let id = people.merge(
            "Ann",
            std::slice::from_ref(&second),
            &[young[15..].iter().collect(), older[15..].iter().collect()],
        );
        assert_eq!(id, first, "the person already called Ann");
        let names: Vec<&str> = people.people.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["Bob", "Ann"], "Annie merged in");
        let ann = &people.people[1];
        assert!(ann.examples.len() <= MAX_EXAMPLES);
        // Each of the four sources kept its share.
        assert_eq!(ann.examples.len(), 4 * (MAX_EXAMPLES / 4));

        // New faces of either look are Ann; Bob's are Bob.
        let new_young = [unit(&[1.0, 0.3, 0.0, 0.05])];
        let new_older = [unit(&[0.6, 0.05, 0.8, 0.0])];
        let groups = vec![
            new_young.iter().collect::<Vec<_>>(),
            new_older.iter().collect(),
            bob.iter().collect(),
        ];
        assert_eq!(assign(&groups, &people, 0.9), [Some(1), Some(1), Some(0)]);

        // Merging unnamed groups alone makes someone new.
        let c = [unit(&[0.0, 1.0, 0.0, 0.0])];
        let id = people.merge("Cat", &[], &[c.iter().collect(), c.iter().collect()]);
        assert_eq!(
            people.people.last().map(|p| (&p.id, p.name.as_str())),
            Some((&id, "Cat"))
        );
    }
}
