//! Keystone: software keystone correction for projectors.
//!
//! Captures a window (game, browser, player), warps it with a perspective
//! transform and shows the result in this window, which you put fullscreen on
//! the projector. Controls mimic a projector menu (H/V keystone) plus free
//! four-corner dragging.

#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod render;
mod source;
mod warp;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use eframe::egui::{self, Color32, Key, Pos2, Rect, Sense, Stroke, Vec2};
use serde::{Deserialize, Serialize};

use render::{Uniforms, WarpCallback, WarpResources};
use source::{ActiveCapture, CaptureShared, SharedFrame, WindowEntry};
use warp::{CORNER_NAMES, KEYSTONE_RANGE, Warp};

const STORAGE_KEY: &str = "keystone-settings";
const PATTERN_SIZE: (u32, u32) = (1920, 1080);
const HANDLE_RADIUS: f32 = 14.0;

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Keystone")
            .with_inner_size([1280.0, 720.0])
            .with_min_inner_size([320.0, 180.0]),
        // We size the window ourselves (to the source resolution); don't restore a stale size.
        persist_window: false,
        ..Default::default()
    };
    eframe::run_native("Keystone", options, Box::new(|cc| Ok(Box::new(KeystoneApp::new(cc)?))))
}

/// Settings persisted between runs.
#[derive(Serialize, Deserialize)]
#[serde(default)]
struct Saved {
    warp: Warp,
    keep_aspect: bool,
    match_source_resolution: bool,
    crop_title_bar: bool,
    capture_cursor: bool,
}

impl Default for Saved {
    fn default() -> Self {
        Self {
            warp: Warp::default(),
            keep_aspect: true,
            match_source_resolution: true,
            crop_title_bar: false,
            capture_cursor: true,
        }
    }
}

#[derive(Clone, PartialEq)]
enum SourceKind {
    Pattern,
    Window(WindowEntry),
}

struct KeystoneApp {
    warp: Warp,
    keep_aspect: bool,
    match_source_resolution: bool,
    capture_cursor: bool,
    crop_title_bar: Arc<AtomicBool>,

    show_controls: bool,
    grid: bool,
    selected_corner: Option<usize>,

    frame: SharedFrame,
    source: SourceKind,
    capture: Option<ActiveCapture>,
    windows: Vec<WindowEntry>,
    status: String,

    /// Last valid shader matrix, kept when the user drags the quad into an invalid shape.
    last_matrix: [[f32; 4]; 3],
    /// Source size we last resized the window for.
    sized_for: Option<(u32, u32)>,
}

impl KeystoneApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Result<Self, String> {
        let rs = cc.wgpu_render_state.as_ref().ok_or("wgpu renderer not available")?;
        WarpResources::install(rs);

        let saved: Saved = cc.storage.and_then(|s| eframe::get_value(s, STORAGE_KEY)).unwrap_or_default();
        let frame = source::new_shared();
        source::write_test_pattern(&frame, PATTERN_SIZE.0, PATTERN_SIZE.1);

        Ok(Self {
            warp: saved.warp,
            keep_aspect: saved.keep_aspect,
            match_source_resolution: saved.match_source_resolution,
            capture_cursor: saved.capture_cursor,
            crop_title_bar: Arc::new(AtomicBool::new(saved.crop_title_bar)),
            show_controls: true,
            grid: false,
            selected_corner: None,
            frame,
            source: SourceKind::Pattern,
            capture: None,
            windows: source::list_windows(),
            status: String::new(),
            last_matrix: warp::shader_matrix(Warp::default().unit_corners()).expect("identity quad is valid"),
            sized_for: None,
        })
    }

    fn set_source(&mut self, ctx: &egui::Context, kind: SourceKind) {
        if let Some(c) = self.capture.take() {
            c.stop();
        }
        self.status.clear();
        self.sized_for = None;
        match &kind {
            SourceKind::Pattern => source::write_test_pattern(&self.frame, PATTERN_SIZE.0, PATTERN_SIZE.1),
            SourceKind::Window(entry) => {
                let shared = CaptureShared {
                    frame: self.frame.clone(),
                    repaint: ctx.clone(),
                    crop_title_bar: self.crop_title_bar.clone(),
                    closed: Arc::new(AtomicBool::new(false)),
                };
                match source::start_capture(entry, shared, self.capture_cursor) {
                    Ok(c) => self.capture = Some(c),
                    Err(e) => {
                        self.status = format!("Capture failed: {e}");
                        source::write_test_pattern(&self.frame, PATTERN_SIZE.0, PATTERN_SIZE.1);
                        self.source = SourceKind::Pattern;
                        return;
                    }
                }
            }
        }
        self.source = kind;
    }

    fn handle_keys(&mut self, ctx: &egui::Context, output_px: Vec2, fit_size: Vec2) {
        // Tab is egui's focus key; take it before any widget sees it.
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, Key::Tab)) {
            self.show_controls = !self.show_controls;
            self.selected_corner = None;
        }
        let fullscreen = ctx.input(|i| i.viewport().fullscreen.unwrap_or(false));
        if ctx.input(|i| i.key_pressed(Key::F11)) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(!fullscreen));
        }
        // Don't steal arrows from a focused slider or text field.
        if ctx.memory(|m| m.focused().is_some()) {
            return;
        }
        if ctx.input(|i| i.key_pressed(Key::Escape)) && self.selected_corner.take().is_none() && fullscreen {
            ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(false));
        }
        if ctx.input(|i| i.key_pressed(Key::G)) {
            self.grid = !self.grid;
        }
        let (dx, dy, shift) = ctx.input(|i| {
            let mut d = (0.0f32, 0.0f32);
            if i.key_pressed(Key::ArrowLeft) {
                d.0 -= 1.0;
            }
            if i.key_pressed(Key::ArrowRight) {
                d.0 += 1.0;
            }
            if i.key_pressed(Key::ArrowUp) {
                d.1 -= 1.0;
            }
            if i.key_pressed(Key::ArrowDown) {
                d.1 += 1.0;
            }
            (d.0, d.1, i.modifiers.shift)
        });
        if dx == 0.0 && dy == 0.0 {
            return;
        }
        if let Some(c) = self.selected_corner {
            // Nudge the selected corner by 1 output pixel (10 with Shift).
            let step = if shift { 10.0 } else { 1.0 };
            let fit_px = fit_size * output_px;
            self.warp.corner_offsets[c][0] += dx * step / fit_px.x.max(1.0);
            self.warp.corner_offsets[c][1] += dy * step / fit_px.y.max(1.0);
        } else {
            // Projector-style: left/right = horizontal keystone, up/down = vertical.
            let step = if shift { 5.0 } else { 1.0 };
            self.warp.horizontal = (self.warp.horizontal + dx * step).clamp(-KEYSTONE_RANGE, KEYSTONE_RANGE);
            self.warp.vertical = (self.warp.vertical - dy * step).clamp(-KEYSTONE_RANGE, KEYSTONE_RANGE);
        }
    }

    fn controls_window(&mut self, ctx: &egui::Context, source_px: Option<(u32, u32)>, output_px: Vec2, valid: bool) {
        egui::Window::new("Keystone")
            .default_pos([16.0, 16.0])
            .resizable(false)
            .collapsible(true)
            .show(ctx, |ui| {
                ui.set_width(340.0);

                // --- Source ---------------------------------------------------
                ui.strong("Source");
                ui.horizontal(|ui| {
                    let current = match &self.source {
                        SourceKind::Pattern => "Calibration pattern".to_string(),
                        SourceKind::Window(w) => short(&w.title, 36),
                    };
                    let mut pick: Option<SourceKind> = None;
                    egui::ComboBox::from_id_salt("source")
                        .width(250.0)
                        .selected_text(current)
                        .show_ui(ui, |ui| {
                            if ui.selectable_label(self.source == SourceKind::Pattern, "Calibration pattern").clicked() {
                                pick = Some(SourceKind::Pattern);
                            }
                            for w in &self.windows {
                                let label = format!("{}  ·  {}", short(&w.title, 48), w.process);
                                let selected = matches!(&self.source, SourceKind::Window(s) if s.hwnd == w.hwnd);
                                if ui.selectable_label(selected, label).clicked() {
                                    pick = Some(SourceKind::Window(w.clone()));
                                }
                            }
                            if self.windows.is_empty() && !cfg!(windows) {
                                ui.weak("Window capture needs Windows");
                            }
                        });
                    if ui.button("⟳").on_hover_text("Refresh window list").clicked() {
                        self.windows = source::list_windows();
                    }
                    if let Some(p) = pick {
                        self.set_source(ui.ctx(), p);
                    }
                });
                let mut crop = self.crop_title_bar.load(Ordering::Relaxed);
                ui.horizontal(|ui| {
                    if ui.checkbox(&mut crop, "Crop title bar").changed() {
                        self.crop_title_bar.store(crop, Ordering::Relaxed);
                    }
                    if ui.checkbox(&mut self.capture_cursor, "Capture cursor").changed() {
                        // Restart the capture so the change applies immediately.
                        if let SourceKind::Window(w) = self.source.clone() {
                            self.set_source(ui.ctx(), SourceKind::Window(w));
                        }
                    }
                });
                if let Some((w, h)) = source_px {
                    ui.weak(format!("Source {w} × {h}   →   output {} × {}", output_px.x.round(), output_px.y.round()));
                    if (w as f32 - output_px.x).abs() > 1.0 || (h as f32 - output_px.y).abs() > 1.0 {
                        ui.weak("Output size differs from source, so the image is rescaled.");
                    }
                }
                if !self.status.is_empty() {
                    ui.colored_label(Color32::from_rgb(255, 170, 60), &self.status);
                }
                ui.separator();

                // --- Projector-style keystone ---------------------------------
                ui.strong("Keystone");
                ui.add(egui::Slider::new(&mut self.warp.horizontal, -KEYSTONE_RANGE..=KEYSTONE_RANGE).step_by(1.0).text("Horizontal"));
                ui.add(egui::Slider::new(&mut self.warp.vertical, -KEYSTONE_RANGE..=KEYSTONE_RANGE).step_by(1.0).text("Vertical"));
                ui.weak("Horizontal +: right edge narrower.  Vertical +: top edge narrower.");
                ui.separator();

                // --- Four corners ---------------------------------------------
                ui.strong("Corners");
                ui.horizontal_wrapped(|ui| {
                    for (i, name) in CORNER_NAMES.iter().enumerate() {
                        let sel = self.selected_corner == Some(i);
                        if ui.selectable_label(sel, *name).clicked() {
                            self.selected_corner = if sel { None } else { Some(i) };
                        }
                    }
                });
                ui.weak("Drag the handles, or select a corner and nudge with the arrow keys (Shift = 10 px).");
                ui.horizontal(|ui| {
                    if ui.button("Reset corners").clicked() {
                        self.warp.corner_offsets = [[0.0; 2]; 4];
                    }
                    if ui.button("Reset keystone").clicked() {
                        self.warp.horizontal = 0.0;
                        self.warp.vertical = 0.0;
                    }
                    if ui.button("Reset all").clicked() {
                        self.warp = Warp::default();
                    }
                });
                if !valid {
                    ui.colored_label(Color32::from_rgb(255, 90, 90), "Shape folds over itself; showing the last valid one.");
                }
                ui.separator();

                // --- Output ---------------------------------------------------
                ui.strong("Output");
                ui.checkbox(&mut self.keep_aspect, "Keep aspect ratio (letterbox)");
                if ui
                    .checkbox(&mut self.match_source_resolution, "Match window to source resolution")
                    .on_hover_text("Sizes this window to the source's pixel size so nothing is rescaled before the warp")
                    .changed()
                {
                    self.sized_for = None;
                }
                ui.checkbox(&mut self.grid, "Alignment grid (G)");
                let fs = ui.ctx().input(|i| i.viewport().fullscreen.unwrap_or(false));
                if ui.button(if fs { "Exit fullscreen (F11)" } else { "Fullscreen (F11)" }).clicked() {
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::Fullscreen(!fs));
                }
                ui.separator();
                ui.weak("Tab: hide/show controls   ←/→: horizontal   ↑/↓: vertical   Esc: deselect / leave fullscreen");
            });
    }
}

impl eframe::App for KeystoneApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();

        // Source window closed under us: fall back to the pattern.
        if self.capture.as_ref().is_some_and(|c| c.closed.load(Ordering::Relaxed)) {
            self.set_source(&ctx, SourceKind::Pattern);
            self.status = "The source window was closed.".into();
        }

        let screen = ctx.content_rect();
        let ppp = ctx.pixels_per_point();
        let output_px = screen.size() * ppp;
        let source_px = render::frame_size(&self.frame);

        // Size the window to the source's native resolution (once per source size).
        let fullscreen = ctx.input(|i| i.viewport().fullscreen.unwrap_or(false));
        if let Some(sz) = source_px
            && self.match_source_resolution
            && !fullscreen
            && self.sized_for != Some(sz)
        {
            self.sized_for = Some(sz);
            ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(Vec2::new(sz.0 as f32, sz.1 as f32) / ppp));
        }

        let src = source_px.map(|(w, h)| [w as f32, h as f32]).unwrap_or([1.0, 1.0]);
        let fit = warp::fit_rect([output_px.x, output_px.y], src, !self.keep_aspect);
        let fit_size = Vec2::new(fit[1][0] - fit[0][0], fit[1][1] - fit[0][1]);

        self.handle_keys(&ctx, output_px, fit_size);

        let out_corners = self.warp.unit_corners().map(|p| warp::unit_to_output(p, fit));
        let matrix = warp::shader_matrix(out_corners);
        let valid = matrix.is_some();
        if let Some(m) = matrix {
            self.last_matrix = m;
        }

        // Warped image, drawn behind everything else.
        let callback = WarpCallback {
            uniforms: Uniforms {
                m: self.last_matrix,
                params: [0.0, 1.0 / output_px.x.max(1.0), 1.0 / output_px.y.max(1.0), if self.grid { 1.0 } else { 0.0 }],
            },
            frame: self.frame.clone(),
        };
        ctx.layer_painter(egui::LayerId::background())
            .add(eframe::egui_wgpu::Callback::new_paint_callback(screen, callback));

        if self.show_controls {
            // Quad outline and draggable corner handles.
            let to_screen = |p: [f32; 2]| Pos2::new(screen.min.x + p[0] * screen.width(), screen.min.y + p[1] * screen.height());
            let pts: Vec<Pos2> = out_corners.iter().map(|p| to_screen(*p)).collect();
            let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Middle, egui::Id::unique("handles")));
            painter.add(egui::Shape::closed_line(pts.clone(), Stroke::new(1.5, Color32::from_rgb(255, 200, 0))));

            for (i, p) in pts.iter().enumerate() {
                let r = Rect::from_center_size(*p, Vec2::splat(HANDLE_RADIUS * 2.0));
                let resp = ui.interact(r, egui::Id::unique(("corner", i)), Sense::click_and_drag());
                if resp.clicked() || resp.drag_started() {
                    self.selected_corner = Some(i);
                }
                if resp.dragged() {
                    let d = resp.drag_delta();
                    let fit_pts = fit_size * screen.size();
                    self.warp.corner_offsets[i][0] += d.x / fit_pts.x.max(1.0);
                    self.warp.corner_offsets[i][1] += d.y / fit_pts.y.max(1.0);
                }
                let selected = self.selected_corner == Some(i);
                let fill = if selected { Color32::from_rgb(255, 200, 0) } else { Color32::from_black_alpha(160) };
                let radius = HANDLE_RADIUS * if resp.hovered() || selected { 1.0 } else { 0.8 };
                painter.circle(*p, radius, fill, Stroke::new(2.0, Color32::from_rgb(255, 200, 0)));
                if resp.hovered() {
                    ctx.set_cursor_icon(egui::CursorIcon::Grab);
                }
            }

            self.controls_window(&ctx, source_px, output_px, valid);
        } else {
            // Clean projector output: no cursor over the picture.
            ctx.set_cursor_icon(egui::CursorIcon::None);
        }
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        let saved = Saved {
            warp: self.warp,
            keep_aspect: self.keep_aspect,
            match_source_resolution: self.match_source_resolution,
            crop_title_bar: self.crop_title_bar.load(Ordering::Relaxed),
            capture_cursor: self.capture_cursor,
        };
        eframe::set_value(storage, STORAGE_KEY, &saved);
    }

    fn on_exit(&mut self) {
        if let Some(c) = self.capture.take() {
            c.stop();
        }
    }
}

fn short(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(max - 1).collect::<String>())
    }
}
