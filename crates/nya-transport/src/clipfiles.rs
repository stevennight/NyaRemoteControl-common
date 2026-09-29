//! Copied files between the two computers (FEATURE_CLIPBOARD_FILES), the
//! part both sides share:
//!
//! * [`Outgoing`]: files copied here, offered to the other side (FileOffer);
//!   the other side asks for them with a FileRequest when they are pasted,
//!   and [`send_items`] sends them.
//! * [`Incoming`]: files copied over there, on our clipboard; when pasted we
//!   request them into a fresh cache folder and wait until every file is in.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::Result;
use nya_proto::pb;
use quinn::Connection;

use crate::files::{self, Item};

/// Offers kept for later paste requests.
const KEEP: usize = 16;

#[derive(Default)]
pub struct Outgoing {
    offers: VecDeque<(u64, Vec<Item>)>,
}

impl Outgoing {
    /// Register copied `paths`; `None` if nothing can be offered. Without
    /// `folders` (older peers) only top-level files are offered.
    pub fn offer(&mut self, paths: &[PathBuf], folders: bool) -> Option<pb::FileOffer> {
        let items: Vec<Item> = if folders {
            files::expand(paths)
        } else {
            files::expand(paths).into_iter().filter(|i| !i.is_dir && !i.rel.contains('/')).collect()
        };
        if items.is_empty() {
            return None;
        }
        let id = rand::random::<u64>();
        let entries = items
            .iter()
            .map(|i| pb::FileEntry {
                name: i.rel.rsplit('/').next().unwrap_or(&i.rel).to_owned(),
                size: i.size,
                path: i.rel.clone(),
                is_dir: i.is_dir,
            })
            .collect();
        self.offers.push_back((id, items));
        while self.offers.len() > KEEP {
            self.offers.pop_front();
        }
        Some(pb::FileOffer { transfer_id: id, files: entries })
    }

    pub fn items(&self, id: u64) -> Option<Vec<Item>> {
        self.offers.iter().find(|(i, _)| *i == id).map(|(_, v)| v.clone())
    }
}

/// Send the files of an offer (folders are implied by the paths).
/// `progress(name, bytes)` is called per chunk.
pub async fn send_items(conn: &Connection, id: u64, items: &[Item], purpose: pb::FilePurpose, mut progress: impl FnMut(&str, u64)) -> Result<()> {
    let list: Vec<&Item> = items.iter().filter(|i| !i.is_dir).collect();
    let count = list.len() as u32;
    for (i, it) in list.iter().enumerate() {
        let name = it.rel.rsplit('/').next().unwrap_or(&it.rel).to_owned();
        let h = pb::FileHeader {
            transfer_id: id,
            name: name.clone(),
            size: it.size,
            purpose: purpose as i32,
            index: i as u32,
            count,
            path: it.rel.clone(),
        };
        files::send_file(conn, h, &it.abs, |n| progress(&name, n)).await?;
    }
    Ok(())
}

/// What to do when something pastes an incoming offer.
#[derive(Debug, PartialEq, Eq)]
pub enum Paste {
    /// Already here: these are the top-level items.
    Ready(Vec<PathBuf>),
    /// Ask the other side for the files (FileRequest, purpose CLIPBOARD).
    Request,
    /// Already requested; wait for the files.
    Wait,
    Failed(String),
    /// Offer unknown or too old.
    Unknown,
}

struct In {
    entries: Vec<pb::FileEntry>,
    root: PathBuf,
    expected: u32,
    received: u32,
    requested: bool,
    result: Option<Result<Vec<PathBuf>, String>>,
}

/// Offers from the other side (shared between the control loop and the file streams).
#[derive(Clone, Default)]
pub struct Incoming(Arc<Mutex<HashMap<u64, In>>>);

impl Incoming {
    /// `cache` is the paste cache folder; this offer is fetched into `cache/<id>`.
    pub fn register(&self, offer: &pb::FileOffer, cache: &Path) {
        let mut m = self.0.lock().unwrap();
        if m.len() >= KEEP {
            // Drop the oldest finished (or never pasted) offers.
            let mut ids: Vec<u64> = m.iter().filter(|(_, v)| !v.requested || v.result.is_some()).map(|(k, _)| *k).collect();
            ids.sort();
            for id in ids.into_iter().take(m.len() + 1 - KEEP) {
                m.remove(&id);
            }
        }
        let expected = offer.files.iter().filter(|f| !f.is_dir).count() as u32;
        m.insert(
            offer.transfer_id,
            In {
                entries: offer.files.clone(),
                root: cache.join(format!("{:016x}", offer.transfer_id)),
                expected,
                received: 0,
                requested: false,
                result: None,
            },
        );
    }

    fn top(e: &In) -> Vec<PathBuf> {
        files::top_level(&e.root, e.entries.iter().map(|f| if f.path.is_empty() { f.name.clone() } else { f.path.clone() }))
    }

    /// Something is pasting offer `id`. The first time, prepares its folder.
    pub fn paste(&self, id: u64) -> Paste {
        let mut m = self.0.lock().unwrap();
        let Some(e) = m.get_mut(&id) else { return Paste::Unknown };
        match &e.result {
            Some(Ok(p)) => return Paste::Ready(p.clone()),
            Some(Err(msg)) => return Paste::Failed(msg.clone()),
            None if e.requested => return Paste::Wait,
            None => {}
        }
        e.requested = true;
        let prepared = std::fs::create_dir_all(&e.root).and_then(|_| {
            for f in e.entries.iter().filter(|f| f.is_dir) {
                std::fs::create_dir_all(e.root.join(files::safe_rel_path(&f.path)))?;
            }
            Ok(())
        });
        if let Err(err) = prepared {
            let msg = format!("无法创建 {}：{err}", e.root.display());
            e.result = Some(Err(msg.clone()));
            return Paste::Failed(msg);
        }
        if e.expected == 0 {
            let top = Self::top(e);
            e.result = Some(Ok(top.clone()));
            return Paste::Ready(top);
        }
        Paste::Request
    }

    /// Folder a file of offer `id` goes to, if it is being pasted.
    pub fn root(&self, id: u64) -> Option<PathBuf> {
        self.0.lock().unwrap().get(&id).filter(|e| e.requested && e.result.is_none()).map(|e| e.root.clone())
    }

    /// One file of `id` arrived (or failed). Returns the outcome once the
    /// whole offer is done.
    pub fn file_done(&self, id: u64, r: Result<(), String>) -> Option<Result<Vec<PathBuf>, String>> {
        let mut m = self.0.lock().unwrap();
        let e = m.get_mut(&id)?;
        if e.result.is_some() {
            return None;
        }
        match r {
            Err(msg) => e.result = Some(Err(msg)),
            Ok(()) => {
                e.received += 1;
                if e.received < e.expected {
                    return None;
                }
                e.result = Some(Ok(Self::top(e)));
            }
        }
        e.result.clone()
    }

    /// The other side could not send offer `id`.
    pub fn fail(&self, id: u64, msg: String) -> Option<Result<Vec<PathBuf>, String>> {
        self.file_done(id, Err(msg))
    }

    /// Everything still waiting fails (the connection ended).
    pub fn fail_all(&self, msg: &str) -> Vec<u64> {
        let mut m = self.0.lock().unwrap();
        let mut ids = Vec::new();
        for (id, e) in m.iter_mut() {
            if e.requested && e.result.is_none() {
                e.result = Some(Err(msg.to_owned()));
                ids.push(*id);
            }
        }
        ids
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(path: &str, size: u64, is_dir: bool) -> pb::FileEntry {
        pb::FileEntry { name: path.rsplit('/').next().unwrap().into(), size, path: path.into(), is_dir }
    }

    #[test]
    fn incoming_lifecycle() {
        let cache = std::env::temp_dir().join(format!("nya-clip-{}", nya_proto::now_us()));
        let inc = Incoming::default();
        let offer = pb::FileOffer {
            transfer_id: 7,
            files: vec![entry("docs", 0, true), entry("docs/empty", 0, true), entry("docs/a.txt", 3, false), entry("b.txt", 1, false)],
        };
        inc.register(&offer, &cache);
        assert_eq!(inc.paste(99), Paste::Unknown);
        assert!(inc.root(7).is_none(), "not requested yet");
        assert_eq!(inc.paste(7), Paste::Request);
        let root = inc.root(7).unwrap();
        assert!(root.join("docs").join("empty").is_dir(), "folders are created up front");
        assert_eq!(inc.paste(7), Paste::Wait);
        assert_eq!(inc.file_done(7, Ok(())), None);
        let done = inc.file_done(7, Ok(())).unwrap().unwrap();
        assert_eq!(done, vec![root.join("docs"), root.join("b.txt")]);
        assert_eq!(inc.paste(7), Paste::Ready(done));
        assert!(inc.root(7).is_none());

        // Only empty folders: ready at once.
        inc.register(&pb::FileOffer { transfer_id: 8, files: vec![entry("x", 0, true)] }, &cache);
        assert!(matches!(inc.paste(8), Paste::Ready(p) if p.len() == 1));

        // Failure is reported once and sticks.
        inc.register(&pb::FileOffer { transfer_id: 9, files: vec![entry("f", 1, false)] }, &cache);
        assert_eq!(inc.paste(9), Paste::Request);
        assert_eq!(inc.fail(9, "boom".into()), Some(Err("boom".into())));
        assert_eq!(inc.file_done(9, Ok(())), None);
        assert_eq!(inc.paste(9), Paste::Failed("boom".into()));
        let _ = std::fs::remove_dir_all(&cache);
    }

    #[test]
    fn outgoing_offers() {
        let base = std::env::temp_dir().join(format!("nya-out-{}", nya_proto::now_us()));
        std::fs::create_dir_all(base.join("d")).unwrap();
        std::fs::write(base.join("d/x"), b"12").unwrap();
        std::fs::write(base.join("y"), b"1").unwrap();
        let mut out = Outgoing::default();
        let o = out.offer(&[base.join("d"), base.join("y")], true).unwrap();
        let paths: Vec<(&str, &str, bool)> = o.files.iter().map(|f| (f.path.as_str(), f.name.as_str(), f.is_dir)).collect();
        assert_eq!(paths, vec![("d", "d", true), ("d/x", "x", false), ("y", "y", false)]);
        assert_eq!(out.items(o.transfer_id).unwrap().len(), 3);
        // Older peers: top-level files only.
        let o = out.offer(&[base.join("d"), base.join("y")], false).unwrap();
        assert_eq!(o.files.len(), 1);
        assert!(out.offer(&[base.join("missing")], true).is_none());
        let _ = std::fs::remove_dir_all(&base);
    }
}
