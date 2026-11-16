use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use anyhow::Result;
use crossbeam_channel::Sender;
use rusqlite::params;
use crate::{app::Ev, db::Db, imaging, repo};

#[derive(Clone, Default, Debug)]
pub struct Stats { pub total: usize, pub done: usize, pub added: usize, pub skipped: usize,
                   pub failed: usize, pub current: String }

pub fn run(db: Arc<Db>, root: PathBuf, screen_side: u32, auto_events: bool,
           cancel: Arc<AtomicBool>, tx: Sender<Ev>, ctx: egui::Context) {
    let files: Vec<PathBuf> = walkdir::WalkDir::new(&root).into_iter()
        .filter_map(|e| e.ok()).filter(|e| e.file_type().is_file())
        .map(|e| e.path().to_path_buf()).filter(|p| imaging::is_supported(p)).collect();

    let mut s = Stats { total: files.len(), ..Default::default() };
    let _ = tx.send(Ev::Import(s.clone()));

    for path in files {
        if cancel.load(Ordering::Relaxed) { break; }
        s.current = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        match one(&db, &root, &path, screen_side, auto_events) {
            Ok(true) => s.added += 1,
            Ok(false) => s.skipped += 1,
            Err(e) => { s.failed += 1; tracing::warn!("import {}: {e:#}", path.display()); }
        }
        s.done += 1;
        if s.done % 5 == 0 || s.done == s.total {
            let _ = tx.send(Ev::Import(s.clone()));
            ctx.request_repaint();
        }
    }
    let _ = tx.send(Ev::ImportDone(s));
    ctx.request_repaint();
}

fn one(db: &Db, root: &PathBuf, path: &PathBuf, side: u32, auto_events: bool) -> Result<bool> {
    let hash = blake3::hash(&std::fs::read(path)?).to_hex().to_string();
    {
        let c = db.conn.lock().unwrap();
        let exists: bool = c.query_row("SELECT EXISTS(SELECT 1 FROM photo WHERE hash=?1)",
                                       [&hash], |r| r.get(0))?;
        if exists { return Ok(false); }
    }
    let m = imaging::meta(path)?;
    let dst = db.store_path(&hash);
    std::fs::create_dir_all(dst.parent().unwrap())?;
    std::fs::copy(path, &dst)?;
    imaging::make_derivatives(&dst, &db.thumb_path(&hash), &db.screen_path(&hash, side), side)?;

    // дата: EXIF → mtime → неизвестно
    let (taken_at, src) = match m.taken_at {
        Some(d) => (Some(d), "exif"),
        None => (std::fs::metadata(path)?.modified().ok()
            .map(|t| chrono::DateTime::<chrono::Local>::from(t).format("%Y-%m-%dT%H:%M:%S").to_string()), "mtime"),
    };
    let rel = format!("store/{}/{}", &hash[..2], hash);
    let c = db.conn.lock().unwrap();
    let tx = c.unchecked_transaction()?;
    tx.execute("INSERT INTO photo(hash,rel_path,bytes,mime,width,height,taken_at,taken_src)
                VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
        params![hash, rel, std::fs::metadata(path)?.len() as i64, imaging::mime_of(path),
                m.w, m.h, taken_at, if taken_at.is_some() { src } else { "unknown" }])?;
    let id = tx.last_insert_rowid();
    if auto_events {
        if let Some(name) = path.parent().and_then(|p| p.strip_prefix(root).ok())
            .and_then(|p| p.components().next())
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .filter(|s| !s.is_empty())
        {
            let ev = repo::ensure_event(&tx, &name)?;
            tx.execute("UPDATE photo SET event_id=?2 WHERE id=?1", params![id, ev])?;
        }
    }
    repo::reindex(&tx, id)?;
    tx.commit()?;
    Ok(true)
}
