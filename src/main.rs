#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::sync::mpsc::{channel, Receiver};
use std::time::Duration;

mod components;
mod core;
mod state;

enum AppEvent {
    Menu(tray_icon::menu::MenuEvent),
    HotKey(global_hotkey::GlobalHotKeyEvent),
}

struct ScreenQrReaderApp {
    _tray_icon: Option<tray_icon::TrayIcon>,
    _hotkey_manager: Option<global_hotkey::GlobalHotKeyManager>,
    hotkey: Option<global_hotkey::hotkey::HotKey>,
    captures: Vec<components::overlay::OverlayState>,
    is_capturing: bool,
    animations_disabled: bool,
    event_rx: Receiver<AppEvent>,
}

impl ScreenQrReaderApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        core::platform::init_window_animation_hook();
        let tray_icon = core::tray::create_tray_menu();
        let (hotkey_manager, hotkey) = core::hotkey::setup_hotkey();

        let (tx, rx) = channel();

        let tx_menu = tx.clone();
        let ctx_menu = cc.egui_ctx.clone();
        tray_icon::menu::MenuEvent::set_event_handler(Some(move |event| {
            let _ = tx_menu.send(AppEvent::Menu(event));
            ctx_menu.request_repaint();
        }));

        let tx_hotkey = tx.clone();
        let ctx_hotkey = cc.egui_ctx.clone();
        global_hotkey::GlobalHotKeyEvent::set_event_handler(Some(move |event| {
            let _ = tx_hotkey.send(AppEvent::HotKey(event));
            ctx_hotkey.request_repaint();
        }));

        Self {
            _tray_icon: tray_icon,
            _hotkey_manager: hotkey_manager,
            hotkey,
            captures: Vec::new(),
            is_capturing: false,
            animations_disabled: false,
            event_rx: rx,
        }
    }

    fn trigger_capture(&mut self) {
        if !core::capture::try_start_capture() {
            return;
        }

        let monitor_captures = core::capture::capture_all_monitors();
        if monitor_captures.is_empty() {
            core::capture::finish_capture();
            return;
        }

        self.captures = monitor_captures
            .into_iter()
            .map(components::overlay::OverlayState::new)
            .collect();
        self.is_capturing = true;
        self.animations_disabled = false;
        core::platform::set_capturing_active(true);
    }

    fn cancel_capture(&mut self) {
        self.captures.clear();
        self.captures.shrink_to_fit();
        self.is_capturing = false;
        self.animations_disabled = false;
        core::platform::set_capturing_active(false);
        core::capture::finish_capture();
        core::platform::trim_memory();
    }
}

impl Drop for ScreenQrReaderApp {
    fn drop(&mut self) {
        tray_icon::menu::MenuEvent::set_event_handler::<fn(tray_icon::menu::MenuEvent)>(None);
        global_hotkey::GlobalHotKeyEvent::set_event_handler::<fn(global_hotkey::GlobalHotKeyEvent)>(None);
        core::platform::cleanup_window_animation_hook();
    }
}

impl eframe::App for ScreenQrReaderApp {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        egui::Rgba::TRANSPARENT.to_array()
    }

    fn ui(&mut self, _ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = _ui.ctx().clone();

        // トレイメニューおよびホットキーイベントの処理
        while let Ok(event) = self.event_rx.try_recv() {
            match event {
                AppEvent::Menu(menu_event) => {
                    match menu_event.id().as_ref() {
                        core::tray::MENU_QUIT_ID => {
                            std::process::exit(0);
                        }
                        core::tray::MENU_CAPTURE_ID => {
                            self.trigger_capture();
                        }
                        _ => {}
                    }
                }
                AppEvent::HotKey(hotkey_event) => {
                    if let Some(hotkey) = &self.hotkey {
                        if hotkey_event.id == hotkey.id() && hotkey_event.state == global_hotkey::HotKeyState::Pressed {
                            self.trigger_capture();
                        }
                    }
                }
            }
        }

        // オーバーレイ描画処理
        if self.is_capturing {
            let mut completed_idx_and_rect = None;
            let mut should_cancel = false;

            for (i, overlay_state) in self.captures.iter_mut().enumerate() {
                let is_ready = overlay_state.is_ready;
                let scale = overlay_state.capture.scale_factor;
                let logical_pos = [
                    overlay_state.capture.x as f32 / scale,
                    overlay_state.capture.y as f32 / scale,
                ];
                let logical_size = [
                    overlay_state.capture.width as f32 / scale,
                    overlay_state.capture.height as f32 / scale,
                ];
                let vp_id = egui::ViewportId::from_hash_of(&format!("overlay_{}", i));
                let vp_builder = egui::ViewportBuilder::default()
                    .with_title(core::platform::OVERLAY_WINDOW_TITLE)
                    .with_visible(is_ready)
                    .with_decorations(false)
                    .with_resizable(false)
                    .with_has_shadow(false)
                    .with_taskbar(false)
                    .with_transparent(false)
                    .with_always_on_top()
                    .with_position(logical_pos)
                    .with_inner_size(logical_size);

                ctx.show_viewport_immediate(vp_id, vp_builder, |vp_ui, _class| {
                    let action = components::overlay::render_overlay(vp_ui, overlay_state);
                    if !is_ready {
                        overlay_state.is_ready = true;
                        vp_ui.ctx().request_repaint();
                        ctx.request_repaint();
                    } else if !overlay_state.is_shown {
                        overlay_state.is_shown = true;
                        if i == 0 {
                            vp_ui.ctx().send_viewport_cmd(egui::ViewportCommand::Focus);
                        }
                        vp_ui.ctx().request_repaint();
                        ctx.request_repaint();
                    }
                    match action {
                        components::overlay::OverlayAction::Complete(rect) => {
                            completed_idx_and_rect = Some((i, rect));
                        }
                        components::overlay::OverlayAction::Cancel => {
                            should_cancel = true;
                        }
                        components::overlay::OverlayAction::None => {}
                    }
                });
            }

            if !self.animations_disabled {
                core::platform::disable_overlay_window_animations();
                self.animations_disabled = true;
            }

            if let Some((idx, rect)) = completed_idx_and_rect {
                if idx < self.captures.len() {
                    let target_overlay = self.captures.swap_remove(idx);
                    self.cancel_capture();
                    ctx.request_repaint();
                    core::qr_action::process_qr_code(&target_overlay.capture.image, &rect);
                    drop(target_overlay);
                    core::platform::trim_memory();
                } else {
                    self.cancel_capture();
                    ctx.request_repaint();
                }
            } else if should_cancel {
                self.cancel_capture();
                ctx.request_repaint();
            }
        }

        // イベント監視のため低頻度で再描画リクエスト
        ctx.request_repaint_after(Duration::from_millis(50));
    }
}

fn main() -> eframe::Result {
    let mut wgpu_options = eframe::egui_wgpu::WgpuConfiguration::default();
    if cfg!(windows) {
        if let eframe::egui_wgpu::WgpuSetup::CreateNew(ref mut setup) = wgpu_options.wgpu_setup {
            setup.instance_descriptor.backends =
                eframe::wgpu::Backends::from_env().unwrap_or(eframe::wgpu::Backends::DX12);
        }
    }

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Screen QR Reader")
            .with_visible(true)
            .with_decorations(false)
            .with_transparent(true)
            .with_taskbar(false)
            .with_has_shadow(false)
            .with_resizable(false)
            .with_inner_size([1.0, 1.0])
            .with_position([-10000.0, -10000.0]),
        wgpu_options,
        ..Default::default()
    };

    eframe::run_native(
        "Screen QR Reader",
        options,
        Box::new(|cc| Ok(Box::new(ScreenQrReaderApp::new(cc)))),
    )
}
