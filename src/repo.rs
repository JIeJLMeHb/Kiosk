use std::collections::HashMap;
use anyhow::Result;
use rusqlite::{params, params_from_iter, Connection, Row, ToSql};
use serde::{de::DeserializeOwned, Serialize};
use crate::model::*;
use rusqlite::OptionalExtension;

const COLS: &str = "p.id,p.hash,p.rel_path,p.width,p.height,p.taken_at,p.taken_src,\
                    p.title,p.note,p.event_id,p.rating,p.hidden";

fn row_photo(r: &Row) -> rusqlite::Result<Photo> {
    Ok(Photo {
        id: r.get(0)?, hash: r.get(1)?, rel_path: r.get(2)?,
        width: r.get(3)?, height: r.get(4)?,
        taken_at: r.get(5)?, taken_src: r.get(6)?,
        title: r.get(7)?, note: r.get(8)?, event_id: r.get(9)?,
        rating: r.get(10)?, hidden: r.get::<_, i64>(11)? != 0,
    })
}

/// Общий построитель фильтра: текст (FTS/LIKE) + даты + событие + теги (И/ИЛИ).
fn filter(q: &Query) -> (String, Vec<Box<dyn ToSql>>) {
    let mut w: Vec<String> = vec!["1=1".into()];
    let mut p: Vec<Box<dyn ToSql>> = vec![];

    if !q.include_hidden { w.push("p.hidden=0".into()); }

    let t = q.text.trim();
    if !t.is_empty() {
        if t.chars().count() >= 3 {
            w.push("p.id IN (SELECT rowid FROM photo_fts WHERE photo_fts MATCH ?)".into());
            p.push(Box::new(format!("\"{}\"", t.replace('"', "\"\""))));
        } else {
            // LIKE в SQLite регистронезависим только для ASCII →search_text хранится в нижнем регистре
            w.push("p.search_text LIKE ?".into());
            p.push(Box::new(format!("%{}%", t.to_lowercase())));
        }
    }
    if let Some(v) = &q.from { w.push("p.taken_day >= ?".into()); p.push(Box::new(v.clone())); }
    if let Some(v) = &q.to   { w.push("p.taken_day <= ?".into()); p.push(Box::new(v.clone())); }
    if let Some(v) = q.event_id { w.push("p.event_id = ?".into()); p.push(Box::new(v)); }

    if !q.tags.is_empty() {
        let ph = vec!["?"; q.tags.len()].join(",");
        let sub = format!("SELECT COUNT(*) FROM photo_tag pt WHERE pt.photo_id=p.id AND pt.tag_id IN ({ph})");
        if q.tags_all { w.push(format!("({sub}) = {}", q.tags.len())); }
        else          { w.push(format!("({sub}) > 0")); }
        for id in &q.tags { p.push(Box::new(*id)); }
    }
    (w.join(" AND "), p)
}

pub fn search(c: &Connection, q: &Query, limit: i64, offset: i64) -> Result<Vec<Photo>> {
    let (wh, mut p) = filter(q);
    let order = match q.order {
        Order::DateDesc => "p.taken_at DESC NULLS LAST, p.id DESC",
        Order::DateAsc  => "p.taken_at ASC  NULLS LAST, p.id ASC",
        Order::ImportDesc => "p.id DESC",
    };
    let sql = format!("SELECT {COLS} FROM photo p WHERE {wh} ORDER BY {order} LIMIT ? OFFSET ?");
    p.push(Box::new(limit)); p.push(Box::new(offset));
    let mut st = c.prepare(&sql)?;
    let rows = st.query_map(params_from_iter(p.iter().map(|b| &**b)), row_photo)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

pub fn count(c: &Connection, q: &Query) -> Result<usize> {
    let (wh, p) = filter(q);
    let sql = format!("SELECT COUNT(*) FROM photo p WHERE {wh}");
    Ok(c.query_row(&sql, params_from_iter(p.iter().map(|b| &**b)), |r| r.get::<_, i64>(0))? as usize)
}

/// Плейлист целиком (≤5000 — держим в памяти). Теги/события берём двумя запросами, без N+1.
pub fn playlist(c: &Connection, q: &Query, order: PlayOrder) -> Result<Vec<Slide>> {
    let photos = search(c, q, i64::MAX, 0)?;
    let mut tags: HashMap<i64, Vec<String>> = HashMap::new();
    {
        let mut st = c.prepare("SELECT pt.photo_id, t.name FROM photo_tag pt JOIN tag t ON t.id=pt.tag_id")?;
        let mut rows = st.query([])?;
        while let Some(r) = rows.next()? {
            tags.entry(r.get(0)?).or_default().push(r.get(1)?);
        }
    }
    let events: HashMap<i64, String> = events(c)?.into_iter().map(|e| (e.id, e.name)).collect();
    let mut v: Vec<Slide> = photos.into_iter().map(|p| Slide {
        id: p.id, hash: p.hash, date: p.taken_at,
        event: p.event_id.and_then(|id| events.get(&id).cloned()).unwrap_or_default(),
        title: p.title, tags: tags.remove(&p.id).unwrap_or_default(),
    }).collect();
    if order == PlayOrder::Shuffle {
        use rand::seq::SliceRandom;
        v.shuffle(&mut rand::thread_rng());
    }
    Ok(v)
}

pub fn all_tags(c: &Connection) -> Result<Vec<Tag>> {
    let mut st = c.prepare("SELECT id,name,kind FROM tag ORDER BY name")?;
    let rows = st.query_map([], |r| Ok(Tag { id: r.get(0)?, name: r.get(1)?, kind: r.get(2)? }))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}
pub fn events(c: &Connection) -> Result<Vec<Event>> {
    let mut st = c.prepare("SELECT id,name FROM event ORDER BY name")?;
    let rows = st.query_map([], |r| Ok(Event { id: r.get(0)?, name: r.get(1)? }))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}
pub fn ensure_tag(c: &Connection, name: &str, kind: &str) -> Result<i64> {
    let name = name.trim();
    c.execute("INSERT INTO tag(name,kind) VALUES(?1,?2) ON CONFLICT(name,kind) DO NOTHING", params![name, kind])?;
    Ok(c.query_row("SELECT id FROM tag WHERE name=?1 AND kind=?2", params![name, kind], |r| r.get(0))?)
}
pub fn ensure_event(c: &Connection, name: &str) -> Result<i64> {
    let name = name.trim();
    c.execute("INSERT INTO event(name) VALUES(?1) ON CONFLICT(name) DO NOTHING", params![name])?;
    Ok(c.query_row("SELECT id FROM event WHERE name=?1", params![name], |r| r.get(0))?)
}
pub fn photo_tags(c: &Connection, id: i64) -> Result<Vec<Tag>> {
    let mut st = c.prepare("SELECT t.id,t.name,t.kind FROM tag t JOIN photo_tag pt ON pt.tag_id=t.id WHERE pt.photo_id=?1 ORDER BY t.name")?;
    let rows = st.query_map([id], |r| Ok(Tag { id: r.get(0)?, name: r.get(1)?, kind: r.get(2)? }))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}
pub fn tag_photos(c: &Connection, photos: &[i64], tag: i64, add: bool) -> Result<()> {
    let tx = c.unchecked_transaction()?;
    for p in photos {
        if add { tx.execute("INSERT OR IGNORE INTO photo_tag VALUES(?1,?2)", params![p, tag])?; }
        else   { tx.execute("DELETE FROM photo_tag WHERE photo_id=?1 AND tag_id=?2", params![p, tag])?; }
        reindex(&tx, *p)?;
    }
    Ok(tx.commit()?)
}
pub fn set_event(c: &Connection, photos: &[i64], ev: Option<i64>) -> Result<()> {
    let tx = c.unchecked_transaction()?;
    for p in photos {
        tx.execute("UPDATE photo SET event_id=?2 WHERE id=?1", params![p, ev])?;
        reindex(&tx, *p)?;
    }
    Ok(tx.commit()?)
}
pub fn set_hidden(c: &Connection, photos: &[i64], hidden: bool) -> Result<()> {
    for p in photos { c.execute("UPDATE photo SET hidden=?2 WHERE id=?1", params![p, hidden as i64])?; }
    Ok(())
}
#[allow(clippy::too_many_arguments)]
pub fn update_photo(c: &Connection, id: i64, title: &str, note: &str, taken_at: Option<&str>,
                    taken_src: &str, event_id: Option<i64>, rating: i32) -> Result<()> {
    c.execute("UPDATE photo SET title=?2,note=?3,taken_at=?4,taken_src=?5,event_id=?6,rating=?7 WHERE id=?1",
              params![id, title, note, taken_at, taken_src, event_id, rating])?;
    reindex(c, id)
}
pub fn photo(c: &Connection, id: i64) -> Result<Photo> {
    Ok(c.query_row(&format!("SELECT {COLS} FROM photo p WHERE p.id=?1"), [id], row_photo)?)
}

/// Единственная точка обновления поискового индекса. Вызывать в той же транзакции, что и правки.
pub fn reindex(c: &Connection, id: i64) -> Result<()> {
    let (title, note, ev): (String, String, String) = c.query_row(
        "SELECT p.title,p.note,COALESCE(e.name,'') FROM photo p LEFT JOIN event e ON e.id=p.event_id WHERE p.id=?1",
        [id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
    let tags = photo_tags(c, id)?.into_iter().map(|t| t.name).collect::<Vec<_>>().join(" ");
    let body = format!("{title} {note} {ev} {tags}").to_lowercase();
    c.execute("UPDATE photo SET search_text=?2 WHERE id=?1", params![id, body])?;
    c.execute("DELETE FROM photo_fts WHERE rowid=?1", [id])?;
    c.execute("INSERT INTO photo_fts(rowid,body) VALUES(?1,?2)", params![id, body])?;
    Ok(())
}

pub fn get_setting<T: DeserializeOwned>(c: &Connection, key: &str) -> Result<Option<T>> {
    let v: Option<String> = c
        .query_row("SELECT value_json FROM settings WHERE key=?1", [key], |r| r.get(0))
        .optional()?;
    match v {
        Some(s) => Ok(Some(serde_json::from_str(&s)?)),
        None => Ok(None),
    }
}

pub fn set_setting<T: Serialize>(c: &Connection, key: &str, v: &T) -> Result<()> {
    c.execute("INSERT INTO settings(key,value_json) VALUES(?1,?2)
               ON CONFLICT(key) DO UPDATE SET value_json=excluded.value_json",
              params![key, serde_json::to_string(v)?])?;
    Ok(())
}
