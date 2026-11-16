#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app; mod auth; mod db; mod imaging; mod import; mod kiosk; mod model; mod player; mod repo;

use std::path::PathBuf;
use std::sync::Arc;
use anyhow::Result;

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let data = args.windows(2).find(|w| w[0] == "--data-dir").map(|w| PathBuf::from(&w[1]))
        .unwrap_or_else(default_data_dir);
    std::fs::create_dir_all(&data)?;

    let (nb, _guard) = tracing_appender::non_blocking(
        tracing_appender::rolling::daily(data.join("logs"), "app.log"));
    tracing_subscriber::fmt().with_writer(nb).with_env_filter(
        tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into())).init();

    let instance = single_instance::SingleInstance::new("PhotoKiosk.Library")?;
    if !instance.is_single() { tracing::warn!("второй экземпляр отклонён"); return Ok(()); }

    let db = Arc::new(db::Db::open(&data)?);
    let (settings, kiosk_enabled) = {
        let c = db.conn.lock().unwrap();
        (repo::get_setting::<model::SlideSettings>(&c, "slide")?.unwrap_or_default(),
         repo::get_setting::<bool>(&c, "kiosk.enabled")?.unwrap_or(false))
    };
    let locked = kiosk_enabled || args.iter().any(|a| a == "--kiosk");
    tracing::info!("старт: data={}, kiosk={}", data.display(), locked);

    let viewport = egui::ViewportBuilder::default()
        .with_title("PhotoKiosk").with_inner_size([1280.0, 800.0])
        .with_fullscreen(locked).with_decorations(!locked);
    let opts = eframe::NativeOptions { viewport, vsync: true, ..Default::default() };

    let res = eframe::run_native("PhotoKiosk", opts, Box::new(move |cc| {
        Ok(Box::new(app::App::new(cc, db, settings, locked)))
    }));

    kiosk::prevent_sleep(false);
    if let Err(e) = res {
        tracing::error!("eframe: {e}");
        anyhow::bail!("eframe: {e}");   // строкой, без конвертации типа ошибки
    }
    Ok(())
}

/// Переносимый режим: если рядом с exe есть data/ — используем её, иначе %LOCALAPPDATA%.
fn default_data_dir() -> PathBuf {
    let exe = std::env::current_exe().unwrap_or_default();
    let beside = exe.parent().map(|p| p.join("data")).unwrap_or_default();
    if beside.join("library.db").exists() { return beside; }
    std::env::var("LOCALAPPDATA").map(|p| PathBuf::from(p).join("PhotoKiosk"))
        .unwrap_or_else(|_| PathBuf::from("./data"))
}
