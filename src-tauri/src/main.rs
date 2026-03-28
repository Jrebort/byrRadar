#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod byr_client;
mod config;
mod http;
mod models;
mod planner;
mod qb_client;
mod runner;

use config::ConfigForm;
use runner::{execute, RunReport};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;
use tauri::{
    image::Image,
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Emitter, Manager, State,
};

const LOG_EVENT: &str = "monitor-log";
const STATE_EVENT: &str = "monitor-state";
const MAX_LOG_LINES: usize = 800;
const PREVIOUS_APP_IDENTIFIER: &str = "com.byrbot.desktop";

#[derive(Debug)]
struct SharedState {
    config_path: PathBuf,
    config: Mutex<ConfigForm>,
    runtime: Mutex<RuntimeState>,
}

impl SharedState {
    fn new(config_path: PathBuf, config: ConfigForm) -> Self {
        Self {
            config_path,
            config: Mutex::new(config),
            runtime: Mutex::new(RuntimeState::default()),
        }
    }
}

#[derive(Debug, Clone)]
struct RuntimeState {
    running: bool,
    paused: bool,
    dry_run: bool,
    status: String,
    report: ReportView,
    logs: Vec<String>,
    last_error: Option<String>,
    stop_flag: Option<Arc<AtomicBool>>,
    pause_flag: Option<Arc<AtomicBool>>,
}

impl Default for RuntimeState {
    fn default() -> Self {
        Self {
            running: false,
            paused: false,
            dry_run: false,
            status: "空闲".to_string(),
            report: ReportView::default(),
            logs: Vec::new(),
            last_error: None,
            stop_flag: None,
            pause_flag: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
struct ReportView {
    free_count: usize,
    filtered_count: usize,
    planned_count: usize,
    rough_planned_count: usize,
    added_count: usize,
    skipped_count: usize,
    duplicate_skip_count: usize,
    budget_bytes: u64,
    free_space_bytes: u64,
    downloading_remaining_bytes: u64,
    seeding_count: usize,
    total_torrents: usize,
    queue_saturated: bool,
}

impl From<&RunReport> for ReportView {
    fn from(report: &RunReport) -> Self {
        Self {
            free_count: report.free_count,
            filtered_count: report.filtered_count,
            planned_count: report.planned_count,
            rough_planned_count: report.rough_planned_count,
            added_count: report.added_count,
            skipped_count: report.skipped_count,
            duplicate_skip_count: report.duplicate_skip_count,
            budget_bytes: report.budget_bytes,
            free_space_bytes: report.free_space_bytes,
            downloading_remaining_bytes: report.downloading_remaining_bytes,
            seeding_count: report.seeding_count,
            total_torrents: report.total_torrents,
            queue_saturated: report.queue_saturated,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct Snapshot {
    config_path: String,
    config: ConfigForm,
    running: bool,
    paused: bool,
    dry_run: bool,
    status: String,
    report: ReportView,
    logs: Vec<String>,
    last_error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct LogPayload {
    line: String,
}

#[tauri::command]
fn load_app_state(state: State<'_, SharedState>) -> Snapshot {
    snapshot_from_state(&state)
}

#[tauri::command]
fn save_config(
    app: AppHandle,
    state: State<'_, SharedState>,
    form: ConfigForm,
) -> Result<Snapshot, String> {
    form.save_to_path(&state.config_path)
        .map_err(|err| err.to_string())?;
    form.apply_process_env();

    {
        let mut current = state.config.lock().expect("config mutex poisoned");
        *current = form;
    }

    clear_error(&app);
    push_log(
        &app,
        format!("已保存配置到 {}", state.config_path.display()),
    );
    emit_snapshot(&app);
    Ok(snapshot_from_state(&state))
}

#[tauri::command]
fn start_monitor(
    app: AppHandle,
    state: State<'_, SharedState>,
    dry_run: bool,
) -> Result<Snapshot, String> {
    let form = {
        let current = state.config.lock().expect("config mutex poisoned");
        current.clone()
    };
    form.apply_process_env();
    let config = form.into_core_config(dry_run).map_err(|err| err.to_string())?;

    let stop_flag = Arc::new(AtomicBool::new(false));
    let pause_flag = Arc::new(AtomicBool::new(false));

    {
        let mut runtime = state.runtime.lock().expect("runtime mutex poisoned");
        if runtime.running {
            return Err("监控已经在运行".to_string());
        }
        runtime.running = true;
        runtime.paused = false;
        runtime.dry_run = dry_run;
        runtime.status = active_status(dry_run).to_string();
        runtime.last_error = None;
        runtime.stop_flag = Some(stop_flag.clone());
        runtime.pause_flag = Some(pause_flag.clone());
    }

    push_log(
        &app,
        if dry_run {
            "开始监控（Dry Run）".to_string()
        } else {
            "开始监控（执行模式）".to_string()
        },
    );
    emit_snapshot(&app);

    let app_handle = app.clone();
    thread::spawn(move || monitor_loop(app_handle, config, stop_flag, pause_flag));

    Ok(snapshot_from_state(&state))
}

#[tauri::command]
fn pause_monitor(app: AppHandle, state: State<'_, SharedState>) -> Snapshot {
    pause_monitor_impl(&app);
    snapshot_from_state(&state)
}

#[tauri::command]
fn resume_monitor(app: AppHandle, state: State<'_, SharedState>) -> Snapshot {
    resume_monitor_impl(&app);
    snapshot_from_state(&state)
}

#[tauri::command]
fn stop_monitor(app: AppHandle, state: State<'_, SharedState>) -> Snapshot {
    stop_monitor_impl(&app);
    snapshot_from_state(&state)
}

fn monitor_loop(
    app: AppHandle,
    config: config::Config,
    stop_flag: Arc<AtomicBool>,
    pause_flag: Arc<AtomicBool>,
) {
    let running_status = active_status(config.dry_run).to_string();

    loop {
        if stop_flag.load(Ordering::Relaxed) {
            break;
        }

        if pause_flag.load(Ordering::Relaxed) {
            set_status(&app, "已暂停监控");
            thread::sleep(Duration::from_millis(300));
            continue;
        }

        set_running_state(&app, &running_status, false);
        push_log(&app, "开始一轮扫描".to_string());

        match execute(&config, |line| push_log(&app, line)) {
            Ok(report) => {
                update_report(&app, &report);
                clear_error(&app);
                set_status(&app, &running_status);
            }
            Err(err) => {
                let message = format!("{err:#}");
                set_error(&app, &message);
                set_status(&app, "失败");
                push_log(&app, format!("运行失败: {message}"));
            }
        }

        emit_snapshot(&app);

        for _ in 0..45 {
            if stop_flag.load(Ordering::Relaxed) || pause_flag.load(Ordering::Relaxed) {
                break;
            }
            thread::sleep(Duration::from_secs(1));
        }
    }

    {
        let state = app.state::<SharedState>();
        let mut runtime = state.runtime.lock().expect("runtime mutex poisoned");
        runtime.running = false;
        runtime.paused = false;
        runtime.dry_run = false;
        runtime.status = "空闲".to_string();
        runtime.stop_flag = None;
        runtime.pause_flag = None;
    }
    push_log(&app, "监控已停止".to_string());
    emit_snapshot(&app);
}

fn pause_monitor_impl(app: &AppHandle) {
    let pause_flag = {
        let state = app.state::<SharedState>();
        let mut runtime = state.runtime.lock().expect("runtime mutex poisoned");
        if !runtime.running || runtime.paused {
            return;
        }
        runtime.paused = true;
        runtime.status = "已暂停监控".to_string();
        runtime.pause_flag.clone()
    };

    if let Some(flag) = pause_flag {
        flag.store(true, Ordering::Relaxed);
    }
    push_log(app, "暂停监控".to_string());
    emit_snapshot(app);
}

fn resume_monitor_impl(app: &AppHandle) {
    let (pause_flag, dry_run) = {
        let state = app.state::<SharedState>();
        let mut runtime = state.runtime.lock().expect("runtime mutex poisoned");
        if !runtime.running || !runtime.paused {
            return;
        }
        runtime.paused = false;
        runtime.status = active_status(runtime.dry_run).to_string();
        (runtime.pause_flag.clone(), runtime.dry_run)
    };

    if let Some(flag) = pause_flag {
        flag.store(false, Ordering::Relaxed);
    }
    push_log(
        app,
        if dry_run {
            "恢复监控（Dry Run）".to_string()
        } else {
            "恢复监控".to_string()
        },
    );
    emit_snapshot(app);
}

fn stop_monitor_impl(app: &AppHandle) {
    let (stop_flag, pause_flag) = {
        let state = app.state::<SharedState>();
        let mut runtime = state.runtime.lock().expect("runtime mutex poisoned");
        if !runtime.running {
            return;
        }
        runtime.status = "停止中".to_string();
        runtime.paused = false;
        (runtime.stop_flag.clone(), runtime.pause_flag.clone())
    };

    if let Some(flag) = pause_flag {
        flag.store(false, Ordering::Relaxed);
    }
    if let Some(flag) = stop_flag {
        flag.store(true, Ordering::Relaxed);
    }
    push_log(app, "停止监控".to_string());
    emit_snapshot(app);
}

fn toggle_pause_from_tray(app: &AppHandle) {
    let paused = {
        let state = app.state::<SharedState>();
        let runtime = state.runtime.lock().expect("runtime mutex poisoned");
        if !runtime.running {
            return;
        }
        runtime.paused
    };

    if paused {
        resume_monitor_impl(app);
    } else {
        pause_monitor_impl(app);
    }
}

fn update_report(app: &AppHandle, report: &RunReport) {
    let state = app.state::<SharedState>();
    let mut runtime = state.runtime.lock().expect("runtime mutex poisoned");
    runtime.report = ReportView::from(report);
}

fn set_status(app: &AppHandle, status: &str) {
    let state = app.state::<SharedState>();
    let mut runtime = state.runtime.lock().expect("runtime mutex poisoned");
    runtime.status = status.to_string();
}

fn set_running_state(app: &AppHandle, status: &str, paused: bool) {
    let state = app.state::<SharedState>();
    let mut runtime = state.runtime.lock().expect("runtime mutex poisoned");
    runtime.status = status.to_string();
    runtime.paused = paused;
}

fn set_error(app: &AppHandle, message: &str) {
    let state = app.state::<SharedState>();
    let mut runtime = state.runtime.lock().expect("runtime mutex poisoned");
    runtime.last_error = Some(message.to_string());
}

fn clear_error(app: &AppHandle) {
    let state = app.state::<SharedState>();
    let mut runtime = state.runtime.lock().expect("runtime mutex poisoned");
    runtime.last_error = None;
}

fn push_log(app: &AppHandle, line: String) {
    {
        let state = app.state::<SharedState>();
        let mut runtime = state.runtime.lock().expect("runtime mutex poisoned");
        runtime.logs.push(line.clone());
        if runtime.logs.len() > MAX_LOG_LINES {
            let extra = runtime.logs.len() - MAX_LOG_LINES;
            runtime.logs.drain(0..extra);
        }
    }
    let _ = app.emit(LOG_EVENT, LogPayload { line });
}

fn emit_snapshot(app: &AppHandle) {
    let snapshot = current_snapshot(app);
    let _ = app.emit(STATE_EVENT, snapshot);
}

fn current_snapshot(app: &AppHandle) -> Snapshot {
    let state = app.state::<SharedState>();
    snapshot_from_state(&state)
}

fn snapshot_from_state(state: &State<'_, SharedState>) -> Snapshot {
    let config = {
        let current = state.config.lock().expect("config mutex poisoned");
        current.clone()
    };
    let runtime = {
        let current = state.runtime.lock().expect("runtime mutex poisoned");
        current.clone()
    };

    Snapshot {
        config_path: state.config_path.display().to_string(),
        config,
        running: runtime.running,
        paused: runtime.paused,
        dry_run: runtime.dry_run,
        status: runtime.status,
        report: runtime.report,
        logs: runtime.logs,
        last_error: runtime.last_error,
    }
}

fn show_main_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

fn active_status(dry_run: bool) -> &'static str {
    if dry_run {
        "监控中 / Dry Run"
    } else {
        "监控中 / 执行"
    }
}

fn build_tray_image() -> Image<'static> {
    let width = 32usize;
    let height = 32usize;
    let mut rgba = vec![0u8; width * height * 4];
    let blend = |dst: &mut [u8], color: (u8, u8, u8, u8)| {
        let alpha = color.3 as f32 / 255.0;
        let inv = 1.0 - alpha;
        dst[0] = (dst[0] as f32 * inv + color.0 as f32 * alpha).round() as u8;
        dst[1] = (dst[1] as f32 * inv + color.1 as f32 * alpha).round() as u8;
        dst[2] = (dst[2] as f32 * inv + color.2 as f32 * alpha).round() as u8;
        dst[3] = ((dst[3] as f32 / 255.0 + alpha * (1.0 - dst[3] as f32 / 255.0)) * 255.0)
            .round() as u8;
    };

    let panel_radius = 8.5f32;
    let center = 15.5f32;
    let radar_r = 9.7f32;

    for y in 0..height {
        for x in 0..width {
            let idx = (y * width + x) * 4;
            let px = x as f32 + 0.5;
            let py = y as f32 + 0.5;

            let dx = (px - center).abs();
            let dy = (py - center).abs();
            let corner_dx = (dx - (15.0 - panel_radius)).max(0.0);
            let corner_dy = (dy - (15.0 - panel_radius)).max(0.0);
            if corner_dx * corner_dx + corner_dy * corner_dy > panel_radius * panel_radius {
                continue;
            }

            let base = if py < center {
                (18, 42, 54, 255)
            } else {
                (14, 31, 42, 255)
            };
            rgba[idx] = base.0;
            rgba[idx + 1] = base.1;
            rgba[idx + 2] = base.2;
            rgba[idx + 3] = base.3;

            let rx = px - center;
            let ry = py - center;
            let distance = (rx * rx + ry * ry).sqrt();

            if distance <= radar_r {
                blend(&mut rgba[idx..idx + 4], (18, 106, 88, 90));
            }

            let angle = ry.atan2(rx).to_degrees();
            let sweep_angle = -22.0f32;
            let angle_delta = (angle - sweep_angle + 540.0) % 360.0 - 180.0;
            if distance <= radar_r && angle_delta.abs() <= 18.0 && angle <= 40.0 {
                let alpha = ((1.0 - distance / radar_r) * 80.0).clamp(18.0, 80.0) as u8;
                blend(&mut rgba[idx..idx + 4], (90, 255, 210, alpha));
            }

            for ring in [3.4f32, 6.6f32, 9.5f32] {
                if (distance - ring).abs() <= 0.45 {
                    blend(&mut rgba[idx..idx + 4], (108, 255, 214, 210));
                }
            }

            if (px - center).abs() <= 0.6 && py >= center && py <= center + radar_r {
                blend(&mut rgba[idx..idx + 4], (122, 255, 220, 230));
            }

            let line_dist = ((ry - rx * sweep_angle.to_radians().tan()).abs()
                / (1.0 + sweep_angle.to_radians().tan().powi(2)).sqrt())
            .abs();
            if rx >= 0.0 && ry <= 0.0 && distance <= radar_r && line_dist <= 0.42 {
                blend(&mut rgba[idx..idx + 4], (255, 188, 88, 255));
            }

            if distance <= 1.25 {
                blend(&mut rgba[idx..idx + 4], (255, 188, 88, 255));
            }

            if (px - 21.0).abs() <= 0.9 && (py - 11.0).abs() <= 0.9 {
                blend(&mut rgba[idx..idx + 4], (126, 255, 224, 255));
            }
        }
    }

    Image::new_owned(rgba, width as u32, height as u32)
}

fn app_config_path(app: &AppHandle) -> PathBuf {
    app.path()
        .app_config_dir()
        .unwrap_or_else(|_| fallback_legacy_config_path())
        .join(".env")
}

fn fallback_legacy_config_path() -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest_dir
        .parent()
        .unwrap_or(Path::new("."))
        .join(".env")
}

fn legacy_config_paths(target_path: &Path) -> Vec<PathBuf> {
    let mut candidates = Vec::new();

    let repo_config = fallback_legacy_config_path();
    if repo_config != target_path {
        candidates.push(repo_config);
    }

    if let Some(app_config_root) = target_path.parent().and_then(|dir| dir.parent()) {
        let old_app_config = app_config_root
            .join(PREVIOUS_APP_IDENTIFIER)
            .join(".env");
        if old_app_config != target_path
            && !candidates.iter().any(|existing| existing == &old_app_config)
        {
            candidates.push(old_app_config);
        }
    }

    if let Ok(current_exe) = std::env::current_exe() {
        if let Some(dir) = current_exe.parent() {
            let path = dir.join(".env");
            if path != target_path && !candidates.iter().any(|existing| existing == &path) {
                candidates.push(path);
            }
        }
    }

    if let Ok(current_dir) = std::env::current_dir() {
        let path = current_dir.join(".env");
        if path != target_path && !candidates.iter().any(|existing| existing == &path) {
            candidates.push(path);
        }
    }

    candidates
}

fn initialize_config(app: &AppHandle) -> (PathBuf, ConfigForm, Vec<String>, Option<String>) {
    let config_path = app_config_path(app);

    if config_path.exists() {
        return match ConfigForm::load_from_path(&config_path) {
            Ok(form) => (config_path, form, Vec::new(), None),
            Err(err) => (
                config_path,
                ConfigForm::default(),
                Vec::new(),
                Some(format!("配置加载失败: {err:#}")),
            ),
        };
    }

    for legacy_path in legacy_config_paths(&config_path) {
        if !legacy_path.exists() {
            continue;
        }

        return match ConfigForm::load_from_path(&legacy_path) {
            Ok(form) => {
                let mut notes = vec![format!(
                    "检测到旧配置，正在迁移到用户目录: {}",
                    config_path.display()
                )];
                let save_result = form.save_to_path(&config_path);
                let error = save_result.err().map(|err| {
                    format!(
                        "已从旧配置 {} 读取，但写入新配置目录失败: {err:#}",
                        legacy_path.display()
                    )
                });
                if error.is_none() {
                    notes.push(format!("配置迁移来源: {}", legacy_path.display()));
                }
                (config_path, form, notes, error)
            }
            Err(err) => (
                config_path,
                ConfigForm::default(),
                Vec::new(),
                Some(format!(
                    "旧配置加载失败 ({}): {err:#}",
                    legacy_path.display()
                )),
            ),
        };
    }

    (
        config_path.clone(),
        ConfigForm::default(),
        vec![format!("首次启动将在此创建配置: {}", config_path.display())],
        None,
    )
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![
            load_app_state,
            save_config,
            start_monitor,
            pause_monitor,
            resume_monitor,
            stop_monitor
        ])
        .setup(|app| {
            let handle = app.handle().clone();
            let (config_path, config, startup_notes, startup_error) = initialize_config(&handle);
            config.apply_process_env();
            app.manage(SharedState::new(config_path.clone(), config));

            push_log(&handle, "Tauri shell ready".to_string());
            push_log(
                &handle,
                format!("配置文件: {}", config_path.display()),
            );
            for note in startup_notes {
                push_log(&handle, note);
            }
            if let Some(message) = startup_error {
                set_error(&handle, &message);
                push_log(&handle, message);
            }

            let show = MenuItem::with_id(app, "show", "显示窗口", true, None::<&str>)?;
            let pause = MenuItem::with_id(app, "pause", "暂停/恢复监控", true, None::<&str>)?;
            let stop = MenuItem::with_id(app, "stop", "停止监控", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&show, &pause, &stop, &quit])?;
            let icon = build_tray_image();

            let tray = TrayIconBuilder::with_id("main-tray")
                .tooltip("byrRadar")
                .icon(icon)
                .menu(&menu)
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "show" => show_main_window(&app.app_handle()),
                    "pause" => toggle_pause_from_tray(&app.app_handle()),
                    "stop" => stop_monitor_impl(&app.app_handle()),
                    "quit" => app.exit(0),
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| {
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } = event
                    {
                        show_main_window(&tray.app_handle());
                    }
                })
                .build(app)?;
            std::mem::forget(tray);

            emit_snapshot(&handle);
            Ok(())
        })
        .on_window_event(|window, event| {
            if window.label() != "main" {
                return;
            }
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
