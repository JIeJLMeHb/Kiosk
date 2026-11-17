use std::collections::HashSet;
use std::path::PathBuf;
use std::time::{Duration, Instant};
use crossbeam_channel::Sender;
use egui::{pos2, vec2, Align2, Color32, ColorImage, FontId, Rect, TextureHandle, TextureOptions};
use crate::{db::Db, model::*};

#[derive(Clone, Copy, PartialEq, Debug)] pub enum ImgKind { Thumb, Screen }
pub struct DecodeReq { pub kind: ImgKind, pub id: i64, pub path: PathBuf }

// Единственное место, где egui рисует текстуру
pub fn paint_image(p: &egui::Painter, tex: &TextureHandle, rect: Rect, uv: Rect, alpha: f32) {
    p.image(tex.id(), rect, uv, Color32::from_white_alpha((alpha.clamp(0.0, 1.0) * 255.0) as u8));
}

struct Frame { id: i64, tex: TextureHandle, w: u32, h: u32 }

pub struct Player {
    pub slides: Vec<Slide>,
    pub idx: usize,
    cur: Option<Frame>, next: Option<Frame>,
    requested: HashSet<i64>, bad: HashSet<i64>,
    slide_started: Instant, fade_started: Option<Instant>,
    pub paused: bool,
    controls_until: Instant,
    gesture: Option<(egui::Pos2, Instant)>,
}

impl Player {
    pub fn new(slides: Vec<Slide>) -> Self {
        Self { slides, idx: 0, cur: None, next: None, requested: HashSet::new(), bad: HashSet::new(),
               slide_started: Instant::now(), fade_started: None, paused: false,
               controls_until: Instant::now(), gesture: None }
    }
    fn at(&self, i: usize) -> &Slide { &self.slides[i % self.slides.len().max(1)] }
    pub fn caption(&self) -> Option<&Slide> { self.slides.get(self.idx) }

    fn request(&mut self, i: usize, db: &Db, s: &SlideSettings, tx: &Sender<DecodeReq>) {
        if self.slides.is_empty() { return; }
        let (id, hash) = { let sl = self.at(i); (sl.id, sl.hash.clone()) };
        if self.bad.contains(&id) || self.requested.contains(&id) { return; }
        if self.cur.as_ref().map(|f| f.id) == Some(id)
            || self.next.as_ref().map(|f| f.id) == Some(id) { return; }
        self.requested.insert(id);
        let mut path = db.screen_path(&hash, s.screen_side);
        if !path.exists() { path = db.store_path(&hash); }
        let _ = tx.send(DecodeReq { kind: ImgKind::Screen, id, path });
    }

    pub fn on_frame(&mut self, ctx: &egui::Context, id: i64, w: u32, h: u32, rgba: &[u8]) {
        self.requested.remove(&id);
        let tex = ctx.load_texture(format!("s{id}"),
            ColorImage::from_rgba_unmultiplied([w as usize, h as usize], rgba), TextureOptions::LINEAR);
        let f = Frame { id, tex, w, h };
        if self.at(self.idx).id == id {
            self.cur = Some(f);
            if self.fade_started.is_none() { self.slide_started = Instant::now(); }
        } else if self.at(self.idx + 1).id == id {
            self.next = Some(f);
        }
    }
    pub fn on_fail(&mut self, id: i64) { self.requested.remove(&id); self.bad.insert(id); }

    fn advance(&mut self) {
        self.idx = (self.idx + 1) % self.slides.len().max(1);
        self.cur = self.next.take();
        self.fade_started = None;
        self.slide_started = Instant::now();
    }
    pub fn jump(&mut self, d: i32) {
        let n = self.slides.len().max(1) as i32;
        self.idx = ((self.idx as i32 + d).rem_euclid(n)) as usize;
        self.cur = None; self.next = None; self.fade_started = None;
        self.slide_started = Instant::now();
        self.controls_until = Instant::now() + Duration::from_secs(4);
    }

    pub fn tick(&mut self, ctx: &egui::Context, s: &SlideSettings, db: &Db, tx: &Sender<DecodeReq>) {
        if self.slides.is_empty() { return; }
        self.request(self.idx, db, s, tx);
        self.request(self.idx + 1, db, s, tx);
        if self.cur.is_none() && self.bad.contains(&self.at(self.idx).id) { self.advance(); }

        let now = Instant::now();
        let dur = Duration::from_secs_f32(s.seconds.max(0.5));
        let fade = Duration::from_millis(s.transition_ms as u64);
        if !self.paused {
            if let Some(t0) = self.fade_started {
                if now.duration_since(t0) >= fade { self.advance(); }
            } else if now.duration_since(self.slide_started) >= dur {
                let nid = self.at(self.idx + 1).id;
                if self.next.as_ref().map(|f| f.id) == Some(nid) { self.fade_started = Some(now); }
            }
        }
        let wait = if self.fade_started.is_some() { Duration::from_millis(16) }
                   else { dur.saturating_sub(now.duration_since(self.slide_started))
                            .min(Duration::from_millis(500)) };
        ctx.request_repaint_after(wait);   // не 60 fps на статичном кадре
    }

    /// Рисует кадр и обрабатывает ввод. Возвращает true, если запрошен выход (нужен пароль).
    pub fn ui(&mut self, ctx: &egui::Context, s: &SlideSettings) -> bool {
        let mut unlock = false;
        ctx.input(|i| {
            if i.key_pressed(egui::Key::Escape) { unlock = true; }
            if i.key_pressed(egui::Key::ArrowRight) { self.jump(1); }
            if i.key_pressed(egui::Key::ArrowLeft) { self.jump(-1); }
            if i.key_pressed(egui::Key::Space) { self.paused = !self.paused; }
        });

        let dur = s.seconds.max(0.5);
        let t_slide = (self.slide_started.elapsed().as_secs_f32() / dur).clamp(0.0, 1.0);

        egui::CentralPanel::default()
            .frame(egui::Frame::default().fill(Color32::BLACK).inner_margin(0.0))
            .show(ctx, |ui| {
                let screen = ui.max_rect();
                let p = ui.painter().clone();
                p.rect_filled(screen, 0.0, Color32::BLACK);

                if let Some(f) = &self.cur {
                    let (r, uv) = geometry(screen, f.w as f32, f.h as f32, s.fit);
                    let uv = if s.transition == Transition::KenBurns && s.fit == Fit::Cover {
                        ken_burns(uv, t_slide, f.id)
                    } else { uv };
                    paint_image(&p, &f.tex, r, uv, 1.0);
                }
                if let (Some(t0), Some(n)) = (self.fade_started, &self.next) {
                    let a = if s.transition_ms == 0 { 1.0 }
                            else { t0.elapsed().as_secs_f32() / (s.transition_ms as f32 / 1000.0) };
                    let (r, uv) = geometry(screen, n.w as f32, n.h as f32, s.fit);
                    paint_image(&p, &n.tex, r, uv, a);
                }
                if self.cur.is_none() {
                    p.text(screen.center(), Align2::CENTER_CENTER, "Загрузка…",
                           FontId::proportional(22.0), Color32::from_gray(140));
                }
                // полоса прогресса
                let w = screen.width() * t_slide;
                p.rect_filled(Rect::from_min_size(screen.left_bottom() - vec2(0.0, 3.0), vec2(w, 3.0)),
                              0.0, Color32::from_white_alpha(60));

                // жесты: свайп = листание, тап = панель управления
                let resp = ui.allocate_rect(screen, egui::Sense::click_and_drag());
                if resp.drag_started() {
                    self.gesture = resp.interact_pointer_pos().map(|p| (p, Instant::now()));
                }
                if resp.drag_stopped() {
                    if let Some((start, t0)) = self.gesture.take() {
                        let end = ctx.input(|i| i.pointer.interact_pos()).unwrap_or(start);
                        let dx = end.x - start.x;
                        if dx.abs() > 60.0 && t0.elapsed() < Duration::from_millis(900) {
                            self.jump(if dx < 0.0 { 1 } else { -1 });
                        }
                    }
                }
                if resp.clicked() { self.controls_until = Instant::now() + Duration::from_secs(5); }
            });

        // подписи
        if let Some(sl) = self.caption() {
            let mut lines = vec![];
            if s.overlay.date { if let Some(d) = &sl.date { lines.push(ru_date(d)); } }
            if s.overlay.event && !sl.event.is_empty() { lines.push(sl.event.clone()); }
            if s.overlay.title && !sl.title.is_empty() { lines.push(sl.title.clone()); }
            if s.overlay.tags && !sl.tags.is_empty() { lines.push(sl.tags.join(" · ")); }
            if !lines.is_empty() {
                egui::Area::new(egui::Id::new("cap")).anchor(Align2::LEFT_BOTTOM, vec2(24.0, -28.0))
                    .interactable(false).show(ctx, |ui| {
                        egui::Frame::popup(ui.style()).fill(Color32::from_black_alpha(140)).show(ui, |ui| {
                            for (i, l) in lines.iter().enumerate() {
                                ui.label(egui::RichText::new(l).size(if i == 0 { 20.0 } else { 16.0 })
                                    .color(Color32::from_gray(235)));
                            }
                        });
                    });
            }
        }

        // панель управления (крупные кнопки под палец)
        if Instant::now() < self.controls_until {
            egui::Area::new(egui::Id::new("ctrl")).anchor(Align2::CENTER_BOTTOM, vec2(0.0, -24.0))
                .show(ctx, |ui| {
                    egui::Frame::popup(ui.style()).show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.spacing_mut().button_padding = vec2(16.0, 12.0);
                            if ui.button("<<").clicked() { self.jump(-1); }
                            if ui.button(if self.paused { "Пуск" } else { "Пауза" }).clicked() { self.paused = !self.paused; }
                            if ui.button(">>").clicked() { self.jump(1); }
                            ui.separator();
                            ui.label(format!("{} / {}", self.idx + 1, self.slides.len()));
                            ui.separator();
                            if ui.button("Выйти").clicked() { unlock = true; }
                        });
                    });
                });
        }
        // курсор прячем, когда нет взаимодействия
        let idle = Instant::now() > self.controls_until;
        ctx.send_viewport_cmd(egui::ViewportCommand::CursorVisible(!idle));
        unlock
    }
}

fn geometry(screen: Rect, iw: f32, ih: f32, fit: Fit) -> (Rect, Rect) {
    let full = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
    match fit {
        Fit::Contain => {
            let k = (screen.width() / iw).min(screen.height() / ih);
            (Rect::from_center_size(screen.center(), vec2(iw * k, ih * k)), full)
        }
        Fit::Cover => {
            let k = (screen.width() / iw).max(screen.height() / ih);
            let uv = Rect::from_center_size(pos2(0.5, 0.5),
                vec2(screen.width() / (iw * k), screen.height() / (ih * k)));
            (screen, uv)
        }
    }
}

fn ken_burns(uv: Rect, t: f32, id: i64) -> Rect {
    let dir = if id % 2 == 0 { 1.0 } else { -1.0 };
    let size = uv.size() * (1.0 - 0.06 * t);
    let c = uv.center() + vec2(0.02 * uv.width() * t * dir, 0.01 * uv.height() * t);
    let mut r = Rect::from_center_size(c, size);
    if r.left() < uv.left() { r = r.translate(vec2(uv.left() - r.left(), 0.0)); }
    if r.right() > uv.right() { r = r.translate(vec2(uv.right() - r.right(), 0.0)); }
    if r.top() < uv.top() { r = r.translate(vec2(0.0, uv.top() - r.top())); }
    if r.bottom() > uv.bottom() { r = r.translate(vec2(0.0, uv.bottom() - r.bottom())); }
    r
}

pub fn ru_date(iso: &str) -> String {
    const M: [&str; 12] = ["января","февраля","марта","апреля","мая","июня",
                           "июля","августа","сентября","октября","ноября","декабря"];
    let b: Vec<&str> = iso.splitn(3, '-').collect();
    if b.len() < 3 { return iso.into(); }
    let d: u32 = b[2][..2].parse().unwrap_or(0);
    let m: usize = b[1].parse().unwrap_or(1);
    format!("{d} {} {}", M[(m - 1).min(11)], b[0])
}
