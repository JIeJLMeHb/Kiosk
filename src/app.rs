use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::{Duration, Instant};
use crossbeam_channel::{Receiver, Sender};
use egui::{Color32, ColorImage, TextureHandle, TextureOptions, ViewportCommand};
use crate::{auth, db::Db, imaging, import, kiosk, model::*, player::*, repo};

pub enum Ev {
    Search { gen: u64, photos: Vec<Photo>, total: usize },
    Image { kind: ImgKind, id: i64, w: u32, h: u32, rgba: Vec<u8> },
    ImageFail { kind: ImgKind, id: i64 },
    Import(import::Stats),
    ImportDone(import::Stats),
    Error(String),
}

enum Dialog { None, Unlock { pw: String, err: String }, SetPw { a: String, b: String, err: String } }
#[derive(PartialEq)] enum Tab { Library, Settings }

struct Edit { id: i64, title: String, note: String, date: String, event: Option<i64>,
              rating: i32, tags: Vec<Tag>, new_tag: String }

pub struct App {
    db: Arc<Db>, ctx: egui::Context,
    tx: Sender<Ev>, rx: Receiver<Ev>,
    hi: Sender<DecodeReq>, lo: Sender<DecodeReq>,
    pool: rayon::ThreadPool,

    tab: Tab,
    query: Query, gen: u64, search_at: Option<Instant>,
    results: Vec<Photo>, total: usize,
    tags: Vec<Tag>, events: Vec<Event>,
    selection: HashSet<i64>, edit: Option<Edit>,
    batch_tag: String,

    thumbs: HashMap<i64, TextureHandle>, thumb_lru: VecDeque<i64>,
    thumb_pending: std::cell::RefCell<HashSet<i64>>,

    settings: SlideSettings,
    player: Option<Player>,
    kiosk_enabled: bool, pw: Option<String>, gate: auth::Gate, dialog: Dialog,
    import: Option<import::Stats>, cancel: Option<Arc<AtomicBool>>,
    toast: Option<(String, Instant)>,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, db: Arc<Db>, settings: SlideSettings, locked: bool) -> Self {
        let ctx = cc.egui_ctx.clone();
        let (tx, rx) = crossbeam_channel::unbounded();
        let (hi, hi_rx) = crossbeam_channel::unbounded::<DecodeReq>();
        let (lo, lo_rx) = crossbeam_channel::unbounded::<DecodeReq>();

        // один поток на кадры плеера (приоритет), два на превью
        for (rx, side) in [(hi_rx.clone(), 0u32), (lo_rx.clone(), imaging::THUMB_SIDE), (lo_rx, imaging::THUMB_SIDE)] {
            let tx = tx.clone(); let ctx = ctx.clone();
            std::thread::spawn(move || while let Ok(r) = rx.recv() {
                let max = if r.kind == ImgKind::Screen { 8192 } else { side };
                match imaging::decode_rgba(&r.path, max) {
                    Ok((w, h, rgba)) => { let _ = tx.send(Ev::Image { kind: r.kind, id: r.id, w, h, rgba }); }
                    Err(e) => { tracing::warn!("decode {}: {e:#}", r.path.display());
                                let _ = tx.send(Ev::ImageFail { kind: r.kind, id: r.id }); }
                }
                ctx.request_repaint();
            });
        }

        let (tags, events, kiosk_enabled, pw) = {
            let c = db.conn.lock().unwrap();
            (repo::all_tags(&c).unwrap_or_default(), repo::events(&c).unwrap_or_default(),
             repo::get_setting::<bool>(&c, "kiosk.enabled").unwrap_or(Some(false)).unwrap(),
             repo::get_setting::<String>(&c, "kiosk.pw").unwrap_or(None))
        };

        let mut app = Self {
            db, ctx: ctx.clone(), tx, rx, hi, lo,
            pool: rayon::ThreadPoolBuilder::new().num_threads(2).build().unwrap(),
            tab: Tab::Library, query: Query::default(), gen: 0, search_at: Some(Instant::now()),
            results: vec![], total: 0, tags, events, selection: HashSet::new(), edit: None,
            batch_tag: String::new(),
            thumbs: HashMap::new(), thumb_lru: VecDeque::new(), thumb_pending: Default::default(),
            settings, player: None, kiosk_enabled, pw, gate: auth::Gate::default(), dialog: Dialog::None,
            import: None, cancel: None, toast: None,
        };
        if locked { app.start_show(true); }
        app
    }

    fn job<F: FnOnce(Arc<Db>, Sender<Ev>, egui::Context) + Send + 'static>(&self, f: F) {
        let (db, tx, ctx) = (self.db.clone(), self.tx.clone(), self.ctx.clone());
        self.pool.spawn(move || { f(db, tx, ctx.clone()); ctx.request_repaint(); });
    }
    fn toast(&mut self, s: impl Into<String>) { self.toast = Some((s.into(), Instant::now())); }
    fn save_settings(&self) {
        let c = self.db.conn.lock().unwrap();
        let _ = repo::set_setting(&c, "slide", &self.settings);
    }

    fn schedule_search(&mut self) { self.gen += 1; self.search_at = Some(Instant::now()); }
    fn maybe_search(&mut self) {
        let Some(t) = self.search_at else { return };
        if t.elapsed() < Duration::from_millis(250) { return; }
        self.search_at = None;
        let (q, gen) = (self.query.clone(), self.gen);
        self.job(move |db, tx, _| {
            let c = db.conn.lock().unwrap();
            match (repo::search(&c, &q, 5000, 0), repo::count(&c, &q)) {
                (Ok(photos), Ok(total)) => { let _ = tx.send(Ev::Search { gen, photos, total }); }
                (Err(e), _) | (_, Err(e)) => { let _ = tx.send(Ev::Error(format!("{e:#}"))); }
            }
        });
    }

    fn thumb(&self, id: i64, hash: &str) -> Option<TextureHandle> {
        if let Some(t) = self.thumbs.get(&id) { return Some(t.clone()); }
        if self.thumb_pending.borrow_mut().insert(id) {
            let _ = self.lo.send(DecodeReq { kind: ImgKind::Thumb, id, path: self.db.thumb_path(hash) });
        }
        None
    }

    fn start_show(&mut self, lock: bool) {
        let q = match &self.settings.source { Source::All => Query::default(), Source::Query(q) => q.clone() };
        let slides = { let c = self.db.conn.lock().unwrap(); repo::playlist(&c, &q, self.settings.order) };
        match slides {
            Ok(s) if !s.is_empty() => {
                self.player = Some(Player::new(s));
                self.ctx.send_viewport_cmd(ViewportCommand::Fullscreen(true));
                self.ctx.send_viewport_cmd(ViewportCommand::Decorations(false));
                kiosk::prevent_sleep(true);
                if lock { kiosk::set_keyboard_hook(true); }
            }
            _ => self.toast("Нечего показывать: фильтр не дал фото"),
        }
    }
    fn stop_show(&mut self) {
        self.player = None;
        self.ctx.send_viewport_cmd(ViewportCommand::Fullscreen(false));
        self.ctx.send_viewport_cmd(ViewportCommand::Decorations(true));
        self.ctx.send_viewport_cmd(ViewportCommand::CursorVisible(true));
        kiosk::prevent_sleep(false);
        kiosk::set_keyboard_hook(false);
    }
    fn request_exit(&mut self) {
        if self.pw.is_some() { self.dialog = Dialog::Unlock { pw: String::new(), err: String::new() }; }
        else { self.stop_show(); }
    }

    fn drain(&mut self, ctx: &egui::Context) {
        while let Ok(ev) = self.rx.try_recv() {
            match ev {
                Ev::Search { gen, photos, total } if gen == self.gen => { self.results = photos; self.total = total; }
                Ev::Search { .. } => {}
                Ev::Image { kind, id, w, h, rgba } => match kind {
                    ImgKind::Screen => if let Some(p) = &mut self.player { p.on_frame(ctx, id, w, h, &rgba); },
                    ImgKind::Thumb => {
                        self.thumb_pending.borrow_mut().remove(&id);
                        let tex = ctx.load_texture(format!("t{id}"),
                            ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &rgba), TextureOptions::LINEAR);
                        self.thumbs.insert(id, tex); self.thumb_lru.push_back(id);
                        while self.thumb_lru.len() > 300 {
                            if let Some(old) = self.thumb_lru.pop_front() { self.thumbs.remove(&old); }
                        }
                    }
                },
                Ev::ImageFail { kind, id } => {
                    self.thumb_pending.borrow_mut().remove(&id);
                    if kind == ImgKind::Screen { if let Some(p) = &mut self.player { p.on_fail(id); } }
                }
                Ev::Import(s) => self.import = Some(s),
                Ev::ImportDone(s) => {
                    self.toast(format!("Импорт: +{}, пропущено {}, ошибок {}", s.added, s.skipped, s.failed));
                    self.import = None; self.cancel = None;
                    let c = self.db.conn.lock().unwrap();
                    self.tags = repo::all_tags(&c).unwrap_or_default();
                    self.events = repo::events(&c).unwrap_or_default();
                    drop(c); self.schedule_search();
                }
                Ev::Error(e) => self.toast(e),
            }
        }
    }

    // ── UI ────────────────────────────────────────────────────────────────
    fn admin_ui(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("top").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.tab, Tab::Library, "Библиотека");
                ui.selectable_value(&mut self.tab, Tab::Settings, "Настройки");
                ui.separator();
                if ui.button("Импорт папки…").clicked() {
                    if let Some(dir) = rfd::FileDialog::new().pick_folder() {
                        let cancel = Arc::new(AtomicBool::new(false));
                        self.cancel = Some(cancel.clone());
                        let side = self.settings.screen_side;
                        self.job(move |db, tx, ctx| import::run(db, dir, side, true, cancel, tx, ctx));
                    }
                }
                if let Some(s) = &self.import {
                    ui.label(format!("{}/{} (+{})", s.done, s.total, s.added));
                    if ui.button("Отмена").clicked() {
                        if let Some(c) = &self.cancel { c.store(true, std::sync::atomic::Ordering::Relaxed); }
                    }
                }
                ui.separator();
                ui.label(format!("Найдено: {}", self.total));
                if ui.button("Показать").clicked() {
                    self.settings.source = Source::Query(self.query.clone());
                    self.save_settings(); self.start_show(false);
                }
                if ui.button("Заблокировать показ").clicked() {
                    if self.pw.is_none() {
                        self.dialog = Dialog::SetPw { a: String::new(), b: String::new(), err: String::new() };
                    } else {
                        self.settings.source = Source::Query(self.query.clone());
                        self.save_settings(); self.start_show(true);
                    }
                }
            });
        });

        if !self.selection.is_empty() { self.batch_bar(ctx); }
        if self.tab == Tab::Library {
            egui::SidePanel::left("filters").resizable(false).default_width(260.0)
                .show(ctx, |ui| self.search_panel(ui));
            egui::CentralPanel::default().show(ctx, |ui| self.grid(ui));
        } else {
            egui::CentralPanel::default().show(ctx, |ui| self.settings_ui(ui));
        }
        self.editor(ctx);
    }

    fn search_panel(&mut self, ui: &mut egui::Ui) {
        let mut changed = false;
        ui.label("Поиск");
        changed |= ui.text_edit_singleline(&mut self.query.text).changed();
        ui.horizontal(|ui| {
            ui.label("Теги:");
            changed |= ui.selectable_value(&mut self.query.tags_all, true, "И").changed();
            changed |= ui.selectable_value(&mut self.query.tags_all, false, "ИЛИ").changed();
        });
        let mut add = None;
        egui::ComboBox::from_id_salt("add_tag").selected_text("добавить тег…").show_ui(ui, |ui| {
            for t in &self.tags { if ui.selectable_label(false, &t.name).clicked() { add = Some(t.id); } }
        });
        if let Some(id) = add { if !self.query.tags.contains(&id) { self.query.tags.push(id); changed = true; } }
        self.query.tags.retain(|id| {
            let name = self.tags.iter().find(|t| t.id == *id).map(|t| t.name.clone()).unwrap_or_default();
            let keep = !ui.button(format!("{name} ×")).clicked();
            keep
        });
        ui.separator();
        let mut from = self.query.from.clone().unwrap_or_default();
        let mut to = self.query.to.clone().unwrap_or_default();
        ui.horizontal(|ui| { ui.label("с"); changed |= ui.text_edit_singleline(&mut from).changed(); });
        ui.horizontal(|ui| { ui.label("по"); changed |= ui.text_edit_singleline(&mut to).changed(); });
        ui.label("ГГГГ-ММ-ДД");
        self.query.from = if from.len() == 10 { Some(from) } else { None };
        self.query.to = if to.len() == 10 { Some(to) } else { None };
        ui.separator();
        let cur = self.query.event_id.and_then(|id| self.events.iter().find(|e| e.id == id))
            .map(|e| e.name.clone()).unwrap_or_else(|| "все события".into());
        egui::ComboBox::from_id_salt("ev").selected_text(cur).show_ui(ui, |ui| {
            if ui.selectable_label(false, "все события").clicked() { self.query.event_id = None; changed = true; }
            for e in &self.events {
                if ui.selectable_label(false, &e.name).clicked() { self.query.event_id = Some(e.id); changed = true; }
            }
        });
        changed |= ui.checkbox(&mut self.query.include_hidden, "показывать скрытые").changed();
        if ui.button("Сбросить").clicked() { self.query = Query::default(); changed = true; }
        if changed { self.schedule_search(); }
    }

    fn grid(&mut self, ui: &mut egui::Ui) {
        let cols = ((ui.available_width() / 190.0).floor() as usize).max(1);
        let rows = (self.results.len() + cols - 1) / cols;
        let mut clicks = vec![];
        egui::ScrollArea::vertical().auto_shrink([false, false]).show_rows(ui, 210.0, rows, |ui, range| {
            for row in range {
                ui.horizontal(|ui| {
                    for col in 0..cols {
                        let i = row * cols + col;
                        if i >= self.results.len() { break; }
                        let p = &self.results[i];
                        let (id, hash, date) = (p.id, p.hash.clone(), p.taken_at.clone().unwrap_or_default());
                        let sel = self.selection.contains(&id);
                        let (rect, resp) = ui.allocate_exact_size(egui::vec2(180.0, 200.0), egui::Sense::click());
                        let paint = ui.painter();
                        paint.rect_filled(rect, 4.0, if sel { Color32::from_rgb(35, 70, 110) } else { Color32::from_gray(28) });
                        let img_rect = rect.shrink(6.0).with_max_y(rect.max.y - 26.0);
                        if let Some(tex) = self.thumb(id, &hash) {
                            let (r, uv) = fit_rect(img_rect, tex.size_vec2());
                            paint_image(paint, &tex, r, uv, 1.0);
                        }
                        paint.text(rect.left_bottom() + egui::vec2(8.0, -18.0), egui::Align2::LEFT_TOP,
                                   &date[..date.len().min(10)], egui::FontId::proportional(12.0), Color32::from_gray(190));
                        if resp.clicked() { clicks.push((id, resp.double_clicked())); }
                    }
                });
            }
        });
        for (id, dbl) in clicks {
            if dbl { self.open_editor(id); }
            else if !self.selection.remove(&id) { self.selection.insert(id); }
        }
    }

    fn batch_bar(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::bottom("batch").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label(format!("Выбрано: {}", self.selection.len()));
                ui.text_edit_singleline(&mut self.batch_tag).on_hover_text("новый или существующий тег");
                let ids: Vec<i64> = self.selection.iter().copied().collect();
                if ui.button("Присвоить тег").clicked() && !self.batch_tag.trim().is_empty() {
                    let c = self.db.conn.lock().unwrap();
                    if let Ok(t) = repo::ensure_tag(&c, &self.batch_tag, "custom") {
                        let _ = repo::tag_photos(&c, &ids, t, true);
                        self.tags = repo::all_tags(&c).unwrap_or_default();
                    }
                    drop(c); self.batch_tag.clear(); self.schedule_search();
                }
                let mut ev = None;
                egui::ComboBox::from_id_salt("batch_ev").selected_text("событие…").show_ui(ui, |ui| {
                    for e in &self.events { if ui.selectable_label(false, &e.name).clicked() { ev = Some(e.id); } }
                });
                if let Some(e) = ev {
                    let c = self.db.conn.lock().unwrap(); let _ = repo::set_event(&c, &ids, Some(e));
                    drop(c); self.schedule_search();
                }
                if ui.button("Скрыть").clicked() {
                    let c = self.db.conn.lock().unwrap(); let _ = repo::set_hidden(&c, &ids, true);
                    drop(c); self.selection.clear(); self.schedule_search();
                }
                if ui.button("Снять выделение").clicked() { self.selection.clear(); }
            });
        });
    }

    fn open_editor(&mut self, id: i64) {
        let c = self.db.conn.lock().unwrap();
        if let Ok(p) = repo::photo(&c, id) {
            self.edit = Some(Edit {
                id, title: p.title, note: p.note, date: p.taken_at.unwrap_or_default(),
                event: p.event_id, rating: p.rating,
                tags: repo::photo_tags(&c, id).unwrap_or_default(), new_tag: String::new(),
            });
        }
    }

    fn editor(&mut self, ctx: &egui::Context) {
        let Some(mut e) = self.edit.take() else { return };
        let mut open = true;        // egui закрывает окно крестиком
        let mut save_close = false; // наш флаг «Сохранить»
        egui::Window::new(format!("Фото #{}", e.id)).open(&mut open).resizable(true).show(ctx, |ui| {
            ui.horizontal(|ui| { ui.label("Заголовок"); ui.text_edit_singleline(&mut e.title); });
            ui.horizontal(|ui| { ui.label("Дата"); ui.text_edit_singleline(&mut e.date);
                                 ui.label("ГГГГ-ММ-ДДTЧЧ:ММ:СС"); });
            ui.label("Заметка"); ui.text_edit_multiline(&mut e.note);
            let cur = e.event.and_then(|id| self.events.iter().find(|x| x.id == id))
                .map(|x| x.name.clone()).unwrap_or_else(|| "—".into());
            egui::ComboBox::from_id_salt("edit_ev").selected_text(cur).show_ui(ui, |ui| {
                if ui.selectable_label(false, "—").clicked() { e.event = None; }
                for x in &self.events {
                    if ui.selectable_label(false, &x.name).clicked() { e.event = Some(x.id); }
                }
            });
            ui.horizontal_wrapped(|ui| {
                let mut rm = None;
                for t in &e.tags { if ui.button(format!("{} ×", t.name)).clicked() { rm = Some(t.id); } }
                if let Some(t) = rm {
                    let c = self.db.conn.lock().unwrap();
                    let _ = repo::tag_photos(&c, &[e.id], t, false);
                    e.tags = repo::photo_tags(&c, e.id).unwrap_or_default();
                }
            });
            ui.horizontal(|ui| {
                ui.text_edit_singleline(&mut e.new_tag);
                if ui.button("Добавить тег").clicked() && !e.new_tag.trim().is_empty() {
                    let c = self.db.conn.lock().unwrap();
                    if let Ok(t) = repo::ensure_tag(&c, &e.new_tag, "custom") {
                        let _ = repo::tag_photos(&c, &[e.id], t, true);
                        e.tags = repo::photo_tags(&c, e.id).unwrap_or_default();
                        self.tags = repo::all_tags(&c).unwrap_or_default();
                    }
                    drop(c);
                    e.new_tag.clear();
                }
            });
            if ui.button("Сохранить").clicked() {
                let c = self.db.conn.lock().unwrap();
                let date = if e.date.len() >= 10 { Some(e.date.as_str()) } else { None };
                let _ = repo::update_photo(&c, e.id, &e.title, &e.note, date, "manual", e.event, e.rating);
                drop(c);                    // ← до вызова метода self
                self.schedule_search();
                save_close = true;
            }
        });
        if open && !save_close { self.edit = Some(e); }
    }

    fn settings_ui(&mut self, ui: &mut egui::Ui) {
        let mut changed = false;
        egui::ScrollArea::vertical().show(ui, |ui| {
            ui.heading("Слайд-шоу");
            changed |= ui.add(egui::Slider::new(&mut self.settings.seconds, 1.0..=120.0).text("секунд на кадр")).changed();
            changed |= ui.add(egui::Slider::new(&mut self.settings.transition_ms, 0..=2000).text("переход, мс")).changed();
            egui::ComboBox::from_label("Переход").selected_text(format!("{:?}", self.settings.transition)).show_ui(ui, |ui| {
                for t in [Transition::None, Transition::Fade, Transition::KenBurns] {
                    changed |= ui.selectable_value(&mut self.settings.transition, t, format!("{t:?}")).changed();
                }
            });
            egui::ComboBox::from_label("Порядок").selected_text(format!("{:?}", self.settings.order)).show_ui(ui, |ui| {
                for o in [PlayOrder::Chrono, PlayOrder::Shuffle] {
                    changed |= ui.selectable_value(&mut self.settings.order, o, format!("{o:?}")).changed();
                }
            });
            egui::ComboBox::from_label("Вписывание").selected_text(format!("{:?}", self.settings.fit)).show_ui(ui, |ui| {
                for f in [Fit::Contain, Fit::Cover] {
                    changed |= ui.selectable_value(&mut self.settings.fit, f, format!("{f:?}")).changed();
                }
            });
            ui.label("Ken Burns работает только в режиме Cover.");
            ui.horizontal(|ui| {
                changed |= ui.checkbox(&mut self.settings.overlay.date, "дата").changed();
                changed |= ui.checkbox(&mut self.settings.overlay.event, "событие").changed();
                changed |= ui.checkbox(&mut self.settings.overlay.title, "заголовок").changed();
                changed |= ui.checkbox(&mut self.settings.overlay.tags, "теги").changed();
            });
            changed |= ui.add(egui::Slider::new(&mut self.settings.screen_side, 1280..=3840).text("кэш экрана, px")).changed();
            ui.label("Смена размера кэша требует перегенерации производных (TODO: кнопка).");

            ui.separator(); ui.heading("Киоск");
            if ui.checkbox(&mut self.kiosk_enabled, "стартовать сразу в заблокированном показе").changed() {
                let c = self.db.conn.lock().unwrap();
                let _ = repo::set_setting(&c, "kiosk.enabled", &self.kiosk_enabled);
                if self.kiosk_enabled && self.pw.is_none() {
                    self.dialog = Dialog::SetPw { a: String::new(), b: String::new(), err: String::new() };
                }
            }
            if ui.button(if self.pw.is_some() { "Сменить пароль" } else { "Установить пароль" }).clicked() {
                self.dialog = Dialog::SetPw { a: String::new(), b: String::new(), err: String::new() };
            }
            if ui.checkbox(&mut { autostart_enabled() }, "запускать при входе в Windows").changed() {
                // состояние чекбокса берём из реестра; см. kiosk::set_autostart
            }
            ui.horizontal(|ui| {
                if ui.button("Включить автозапуск").clicked() { let _ = kiosk::set_autostart(true); }
                if ui.button("Выключить автозапуск").clicked() { let _ = kiosk::set_autostart(false); }
            });
            ui.colored_label(Color32::from_rgb(220, 170, 60),
                "Пароль защищает от обычного пользователя. Alt+Tab/Ctrl+Alt+Del блокируются только средствами ОС — см. README.");
        });
        if changed { self.save_settings(); }
    }

    fn dialogs(&mut self, ctx: &egui::Context) {
        let d = std::mem::replace(&mut self.dialog, Dialog::None);
        match d {
            Dialog::None => {}
            Dialog::Unlock { mut pw, mut err } => {
                let wait = self.gate.wait_secs();
                let mut close = false;
                egui::Window::new("Выход из показа").collapsible(false).resizable(false)
                    .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0]).show(ctx, |ui| {
                        ui.label("Введите пароль администратора");
                        let r = ui.add(egui::TextEdit::singleline(&mut pw).password(true).desired_width(240.0));
                        r.request_focus();
                        if wait > 0 { ui.colored_label(Color32::RED, format!("Подождите {wait} с")); }
                        if !err.is_empty() { ui.colored_label(Color32::RED, &err); }
                        ui.horizontal(|ui| {
                            let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
                            if (ui.button("Разблокировать").clicked() || enter) && self.gate.allowed() {
                                if auth::verify(pw.clone(), self.pw.as_deref().unwrap_or("")) {
                                    self.gate.ok(); self.stop_show(); close = true;
                                } else { self.gate.fail(); err = "Неверный пароль".into(); pw.clear(); }
                            }
                            if ui.button("Отмена").clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)) { close = true; }
                        });
                    });
                if !close { self.dialog = Dialog::Unlock { pw, err }; }
            }
            Dialog::SetPw { mut a, mut b, mut err } => {
                let mut close = false;
                egui::Window::new("Пароль киоска").collapsible(false).show(ctx, |ui| {
                    ui.add(egui::TextEdit::singleline(&mut a).password(true).hint_text("новый пароль"));
                    ui.add(egui::TextEdit::singleline(&mut b).password(true).hint_text("повторите"));
                    if !err.is_empty() { ui.colored_label(Color32::RED, &err); }
                    ui.horizontal(|ui| {
                        if ui.button("Сохранить").clicked() {
                            if a.len() < 4 { err = "Минимум 4 символа".into(); }
                            else if a != b { err = "Пароли не совпадают".into(); }
                            else {
                                match auth::hash(a.clone()) {
                                    Ok(h) => {
                                        let c = self.db.conn.lock().unwrap();
                                        let _ = repo::set_setting(&c, "kiosk.pw", &h);
                                        drop(c);                       // ← добавить
                                        self.pw = Some(h);
                                        close = true;
                                        self.toast("Пароль сохранён");
                                    }
                                    Err(e) => err = format!("{e:#}"),
                                }
                            }
                        }
                        if ui.button("Отмена").clicked() { close = true; }
                    });
                });
                if !close { self.dialog = Dialog::SetPw { a, b, err }; }
            }
        }
    }
}

fn autostart_enabled() -> bool { false } // TODO: чтение HKCU\...\Run для состояния чекбокса

fn fit_rect(area: egui::Rect, size: egui::Vec2) -> (egui::Rect, egui::Rect) {
    let k = (area.width() / size.x).min(area.height() / size.y);
    (egui::Rect::from_center_size(area.center(), size * k),
     egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)))
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _: &mut eframe::Frame) {
        self.drain(ctx);
        self.maybe_search();
        let modal = !matches!(self.dialog, Dialog::None);
        if self.player.is_some() {
            let (s, db, hi) = (self.settings.clone(), self.db.clone(), self.hi.clone());
            let p = self.player.as_mut().unwrap();
            p.tick(ctx, &s, &db, &hi);
            let unlock = if modal { p.ui(ctx, &s); false } else { p.ui(ctx, &s) };
            if unlock { self.request_exit(); }
        } else {
            self.admin_ui(ctx);
        }
        self.dialogs(ctx);
        if let Some((msg, t)) = &self.toast {
            if t.elapsed() < Duration::from_secs(5) {
                let msg = msg.clone();
                egui::Area::new(egui::Id::new("toast")).anchor(egui::Align2::RIGHT_TOP, [-16.0, 16.0])
                    .show(ctx, |ui| { egui::Frame::popup(ui.style()).show(ui, |ui| ui.label(msg)); });
            } else { self.toast = None; }
        }
    }
}
