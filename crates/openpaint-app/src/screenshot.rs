//! Looking at the panel UI without running the application.
//!
//! **This exists because several bugs shipped that a glance would have caught.** The panel padding
//! was applied twice, so a menu strip was mostly made of nothing; a tool rail lost a whole band of
//! height to a header that belonged on its side. Both were obvious on screen and invisible to
//! every test, because the tests check rectangles and nobody was looking at pixels.
//!
//! So: a headless device, a synthetic egui frame, and a PNG. Test-only, and deliberately not
//! assertions — these are for a person to open. A golden-image comparison would fail on every font
//! and driver difference and teach everyone to ignore it.
//!
//! It also makes [`crate::panel_draw`] testable at all. `show` needs an `egui::Ui`, so until now
//! nothing checked what a press on a control actually does end to end — only what `panel_ui` says
//! it should. [`frame_with`] closes that: it drives a real egui frame with synthetic pointer
//! events and no window anywhere.

use crate::layout::Rect;
use crate::panel_ui::Control;
use crate::panel_ui::Direction;
use crate::theme::Theme;
use crate::workspace::{Place, Workspace};

/// Where the images land.
const DIR: &str = "target/screenshots";

/// A pointer event to feed a synthetic frame.
#[derive(Clone, Copy, Debug)]
pub enum Poke {
    Move(f32, f32),
    Press(f32, f32),
    Release(f32, f32),
    /// Words typed, as the window would deliver them.
    Say(&'static str),
    /// A key pressed and let go: Enter, Backspace, an arrow.
    Tap(egui::Key),
}

/// Run egui frames with no window, and hand back what the last one drew.
///
/// **Two frames, always.** egui sizes text from a font atlas it builds as it goes, so the first
/// frame after a context is created can measure everything as zero — and this UI *branches* on
/// measured width: `Direction::Auto` lays a strip out across or down depending on whether it fits.
/// A single frame can therefore produce a picture that is plausible and wrong. The second frame
/// sees the atlas the first one filled.
///
/// **Every pass is returned, not just the last.** egui reports its font atlas as a *delta*: the
/// first frame builds it and hands it over, and later frames report nothing because nothing
/// changed. Keeping only the final output therefore means uploading no font texture at all, and
/// the renderer quietly draws nothing — the first version of this produced a picture containing
/// only its own clear colour, which looked exactly like a UI that had failed to lay out.
fn run_frames(
    ctx: &egui::Context,
    screen: Rect,
    pokes: &[Poke],
    mut build: impl FnMut(&egui::Context),
) -> Vec<egui::FullOutput> {
    let rect = egui::Rect::from_min_size(
        egui::pos2(screen.x, screen.y),
        egui::vec2(screen.w, screen.h),
    );
    let mut passes = Vec::new();
    for pass in 0..2 {
        let mut input = egui::RawInput {
            screen_rect: Some(rect),
            ..Default::default()
        };
        // Events only on the final pass: replaying a press twice would toggle whatever it hit
        // back to where it started, which is the sort of thing that makes a harness lie.
        if pass == 1 {
            for poke in pokes {
                input.events.push(as_event(*poke));
            }
        }
        passes.push(ctx.run(input, &mut build));
    }
    passes
}

/// One poke, as the window would have delivered it.
#[must_use]
fn as_event(poke: Poke) -> egui::Event {
    match poke {
        Poke::Move(x, y) => egui::Event::PointerMoved(egui::pos2(x, y)),
        Poke::Press(x, y) | Poke::Release(x, y) => egui::Event::PointerButton {
            pos: egui::pos2(x, y),
            button: egui::PointerButton::Primary,
            pressed: matches!(poke, Poke::Press(..)),
            modifiers: egui::Modifiers::default(),
        },
        Poke::Say(text) => egui::Event::Text(text.to_owned()),
        Poke::Tap(key) => egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::default(),
        },
    }
}

/// Drive one settled egui frame and let the caller inspect whatever the closure produced.
///
/// The way to test [`crate::panel_draw::show`] without a window: build a `Ui`, draw controls into
/// it, and read back what the press did.
pub fn frame_with<T>(
    screen: Rect,
    pokes: &[Poke],
    mut build: impl FnMut(&egui::Context) -> T,
) -> T {
    let ctx = egui::Context::default();
    let mut out = None;
    let _ = run_frames(&ctx, screen, pokes, |c| out = Some(build(c)));
    out.expect("the closure runs on every pass")
}

/// Draw a list of controls into a real egui frame and report what a press did.
///
/// The missing half of `panel_ui`'s tests: those prove what a press at a point *should* mean, this
/// proves that pressing there produces it once egui, hit-testing and the renderer are in the way.
#[must_use]
pub fn press_controls(
    controls: &[Control],
    area: Rect,
    direction: Direction,
    at: (f32, f32),
) -> (Vec<crate::panel_ui::Change>, crate::panel_draw::PanelInput) {
    let theme = Theme::default();
    let mut input = crate::panel_draw::PanelInput::default();
    let mut changes = Vec::new();
    let screen = Rect::new(0.0, 0.0, area.x + area.w + 40.0, area.y + area.h + 40.0);
    frame_with(
        screen,
        &[
            Poke::Move(at.0, at.1),
            Poke::Press(at.0, at.1),
            Poke::Release(at.0, at.1),
        ],
        |ctx| {
            let mut ui = egui::Ui::new(
                ctx.clone(),
                egui::LayerId::new(egui::Order::Middle, egui::Id::new("shot")),
                egui::Id::new("shot-ui"),
                egui::UiBuilder::new().max_rect(egui::Rect::from_min_size(
                    egui::pos2(area.x, area.y),
                    egui::vec2(area.w, area.h),
                )),
            );
            changes = crate::panel_draw::show(&mut ui, controls, &theme, direction, &mut input);
        },
    );
    (changes, input)
}

/// A document to draw the panels against.
///
/// **Not `Default`, because there is no `Default`** -- and there should not be: every field of
/// `Status` is the document saying something about itself, and a blanket default would be a
/// second, quieter answer to all of it. Written out here, once, with enough in it that the panels
/// have something to show: layers with names, a palette, a page, a font that was substituted.
#[must_use]
pub(crate) fn sample_document() -> (
    Vec<openpaint_core::Layer>,
    Vec<[u8; 3]>,
    Vec<openpaint_core::BrushPreset>,
    Vec<String>,
) {
    let mut layers: Vec<openpaint_core::Layer> = ["Paper", "Flats", "Ink"]
        .iter()
        .enumerate()
        .map(|(i, name)| {
            openpaint_core::Layer::restored(
                u32::try_from(i).expect("three of them"),
                *name,
                1.0,
                openpaint_core::Blend::Normal,
                true,
                false,
                false,
            )
        })
        .collect();
    // **And one of them is lettering**, because a fixture with no text layer cannot draw the
    // caption editor -- and a fixture that cannot draw a thing cannot fail on it. That is not
    // hypothetical: the font-substitution warning was emitted twice, the check for it went in,
    // the fault was put back on purpose, and every test stayed green because none of them had a
    // text layer to reach the second copy with.
    layers.push(
        openpaint_core::Layer::restored(
            3,
            "Caption",
            1.0,
            openpaint_core::Blend::Normal,
            true,
            false,
            false,
        )
        .with_text(openpaint_core::TextBlock {
            text: "Be here".to_owned(),
            ..openpaint_core::TextBlock::default()
        }),
    );
    let palette = vec![
        [232, 196, 168],
        [186, 132, 102],
        [64, 48, 56],
        [240, 240, 236],
        [120, 160, 200],
    ];
    let presets = Vec::new();
    let fonts = vec!["Inter".to_owned(), "Source Han Sans".to_owned()];
    (layers, palette, presets, fonts)
}

/// Render one frame of a workspace with the **real** panels in it.
///
/// [`shoot`] draws `filler`, which is right for looking at chrome and wrong for looking at panels:
/// a picture of stand-in controls under a panel's name says nothing about that panel and says it
/// convincingly. This one calls `panels::show`, the same way the application does.
pub fn shoot_panels(name: &str, screen: Rect, ws: &mut Workspace) {
    let Some((device, queue)) = crate::test_gpu::try_device() else {
        eprintln!("screenshot {name}: no GPU adapter, skipped");
        return;
    };
    let (layers, palette, presets, fonts) = sample_document();
    let theme = ws.theme;
    let mut brush = openpaint_core::Brush::default();
    let mut colour = brush.color_srgb8();
    let mut panel_input: std::collections::HashMap<u32, crate::panel_draw::PanelInput> =
        std::collections::HashMap::new();
    let mut menu: Option<u32> = None;
    let mut pick: Option<crate::panel_ui::ControlId> = None;
    let mut wheel_shape = crate::colour_wheel::Shape::default();
    let mut wheel_hold: Option<crate::colour_wheel::Region> = None;

    let ctx = egui::Context::default();
    let mut passes = run_frames(&ctx, screen, &[], |c| {
        let status = crate::ui::Status {
            history: (3, 1, 2 * 1024 * 1024),
            message: None,
            page_size: (1200, 1600),
            crop: None,
            crop_rect: None,
            residency: (0, 0),
            spilled: 0,
            traffic: (0, 0),
            layers: &layers,
            active_layer: 2,
            pages: (3, 0),
            tool: crate::editor::Tool::Brush,
            confirm: None,
            brush_cursor: None,
            perf: crate::perf::PerfSnapshot::default(),
            recovery: None,
            palette: &palette,
            presets: &presets,
            preset_trouble: None,
            font_families: &fonts,
            font_substituted: None,
            transform: None,
            transform_box: None,
            kernel: openpaint_core::Kernel::default(),
            autosave: "",
            export: None,
            selection: &[],
            select_tool: Some(crate::ui::SelectTool::Wand),
            has_selection: true,
            wand: crate::ui::WandSettings::default(),
        };
        ws.show(
            c,
            screen,
            crate::workspace::Attention::Workspace,
            |panel, ui, direction, place| {
                let mut paint = crate::ui::Painting {
                    theme: &theme,
                    direction,
                    input: panel_input.entry(panel.0).or_default(),
                    menu: &mut menu,
                    pick: &mut pick,
                    extend_by: 512,
                    preset_name: "",
                    ctx: c,
                    wheel_shape: &mut wheel_shape,
                    wheel_hold: &mut wheel_hold,
                };
                crate::panels::show(
                    panel,
                    ui,
                    &mut brush,
                    &mut colour,
                    &status,
                    &mut paint,
                    place,
                );
            },
        );
    });

    let mut renderer = egui_wgpu::Renderer::new(device, crate::test_gpu::SURFACE, None, 1, true);
    for pass in &passes {
        for (id, delta) in &pass.textures_delta.set {
            renderer.update_texture(device, queue, *id, delta);
        }
    }
    let output = passes.pop().expect("at least one pass");
    render_to_png(name, screen, device, queue, &mut renderer, &ctx, output);
}

/// A workspace with the built-in arrangement and the built-in look.
///
/// **Not `Workspace::default()`**, which reads the saved layout and theme out of the real data
/// directory: a screenshot of "the default workspace" would then be a screenshot of whatever the
/// machine running it happened to have saved, and would change under you for no reason.
#[must_use]
pub fn plain_workspace() -> Workspace {
    // Everything the saved workspace could have supplied, put back to the built-in answer.
    // Resetting only the layout was not enough: floating panels came off disk too, so a test that
    // floated one found it already floating, quietly did nothing, and drew somebody else's
    // workspace.
    let mut ws = Workspace::default();
    ws.reset_to_built_in();
    ws
}

/// Render one frame of a workspace to `target/screenshots/<name>.png`.
///
/// Skips, loudly, where there is no usable GPU — the same rule the other GPU tests follow, since a
/// hard failure on a machine without an adapter says nothing about the code.
pub fn shoot(name: &str, screen: Rect, pokes: &[Poke], ws: &mut Workspace) {
    let Some((device, queue)) = crate::test_gpu::try_device() else {
        eprintln!("screenshot {name}: no GPU adapter, skipped");
        return;
    };
    let ctx = egui::Context::default();
    let mut passes = run_frames(&ctx, screen, pokes, |c| {
        ws.show(
            c,
            screen,
            crate::workspace::Attention::Workspace,
            |panel, ui, direction, place| {
                filler(panel, ui, direction, place);
            },
        );
    });

    let mut renderer = egui_wgpu::Renderer::new(device, crate::test_gpu::SURFACE, None, 1, true);
    // Every pass's textures, in order: the font atlas arrives with the first one and never again.
    for pass in &passes {
        for (id, delta) in &pass.textures_delta.set {
            renderer.update_texture(device, queue, *id, delta);
        }
    }
    let output = passes.pop().expect("at least one pass");
    render_to_png(name, screen, device, queue, &mut renderer, &ctx, output);
}

/// Rasterise a finished egui frame and write it out.
///
/// Split from `shoot` so anything that draws its own frame -- a sheet of every icon, say -- can
/// reuse it rather than growing a second copy that drifts.
fn render_to_png(
    name: &str,
    screen: Rect,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    renderer: &mut egui_wgpu::Renderer,
    ctx: &egui::Context,
    output: egui::FullOutput,
) {
    let (w, h) = (screen.w as u32, screen.h as u32);
    let primitives = ctx.tessellate(output.shapes, output.pixels_per_point);
    let desc = egui_wgpu::ScreenDescriptor {
        size_in_pixels: [w, h],
        pixels_per_point: output.pixels_per_point,
    };

    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("screenshot"),
        size: wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: crate::test_gpu::SURFACE,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());

    let mut encoder = device.create_command_encoder(&Default::default());
    renderer.update_buffers(device, queue, &mut encoder, &primitives, &desc);
    {
        // Cleared rather than loaded, unlike the real path: on screen egui draws over a canvas the
        // GPU has already put there, and here there is nothing underneath. The clear colour is the
        // canvas colour so a missing panel reads as a hole rather than as black.
        let ground = Theme::default().palette.canvas.0;
        let mut pass = encoder
            .begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("screenshot"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: f64::from(ground[0]) / 255.0,
                            g: f64::from(ground[1]) / 255.0,
                            b: f64::from(ground[2]) / 255.0,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            })
            .forget_lifetime();
        renderer.render(&mut pass, &primitives, &desc);
    }

    // A copy's rows must start on a 256-byte boundary, so the buffer is padded and the padding is
    // dropped on the way out.
    let unpadded = w * 4;
    let padded = unpadded.div_ceil(256) * 256;
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("screenshot-readback"),
        size: u64::from(padded) * u64::from(h),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    encoder.copy_texture_to_buffer(
        wgpu::ImageCopyTexture {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::ImageCopyBuffer {
            buffer: &buffer,
            layout: wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(padded),
                rows_per_image: Some(h),
            },
        },
        wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
    );
    queue.submit([encoder.finish()]);

    let slice = buffer.slice(..);
    slice.map_async(wgpu::MapMode::Read, |_| {});
    device.poll(wgpu::Maintain::Wait);
    let mapped = slice.get_mapped_range();
    let mut rgba = Vec::with_capacity((unpadded * h) as usize);
    for row in 0..h {
        let start = (row * padded) as usize;
        rgba.extend_from_slice(&mapped[start..start + unpadded as usize]);
    }
    drop(mapped);
    buffer.unmap();

    if let Err(e) = write_png(name, w, h, &rgba) {
        // Loudly: a harness that silently writes nothing is worse than no harness, because you go
        // looking at a stale image and believe it.
        panic!("screenshot {name}: could not write the image ({e})");
    }
    println!("screenshot: {DIR}/{name}.png");
}

fn write_png(name: &str, w: u32, h: u32, rgba: &[u8]) -> Result<(), String> {
    std::fs::create_dir_all(DIR).map_err(|e| e.to_string())?;
    let path = std::path::Path::new(DIR).join(format!("{name}.png"));
    let file = std::fs::File::create(&path).map_err(|e| e.to_string())?;
    let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), w, h);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.set_source_srgb(png::SrgbRenderingIntent::Perceptual);
    let mut writer = encoder.write_header().map_err(|e| e.to_string())?;
    writer.write_image_data(rgba).map_err(|e| e.to_string())
}

/// Stand-in panel contents.
///
/// The real contents live in `ui.rs` and need a document, a brush and an editor behind them. What
/// a screenshot is for is the *chrome* — headers, tabs, dividers, the ground between panels, and
/// how a strip lays itself out — so the contents only have to be something with a shape.
fn filler(panel: crate::layout::PanelId, ui: &mut egui::Ui, direction: Direction, place: Place) {
    if panel == crate::workspace::CANVAS || place == Place::Popup {
        return;
    }
    let theme = Theme::default();
    let controls = vec![
        Control::Label {
            text: format!("Panel {}", panel.0),
        },
        Control::Slider {
            id: 0,
            text: "Size".to_owned(),
            value: 14.0,
            min: 0.5,
            max: 400.0,
            unit: "px",
            log: true,
        },
        Control::Separator,
        Control::Choice {
            id: 1,
            text: "Brush".to_owned(),
            selected: true,
            icon: Some(crate::icons::Symbol::Brush),
        },
        Control::Choice {
            id: 2,
            text: "Eraser".to_owned(),
            selected: false,
            icon: Some(crate::icons::Symbol::Eraser),
        },
        Control::Choice {
            id: 3,
            text: "Lasso".to_owned(),
            selected: false,
            icon: Some(crate::icons::Symbol::Lasso),
        },
        Control::Choice {
            id: 4,
            text: "Wand".to_owned(),
            selected: false,
            icon: Some(crate::icons::Symbol::Wand),
        },
    ];
    let mut input = crate::panel_draw::PanelInput::default();
    let _ = crate::panel_draw::show(ui, &controls, &theme, direction, &mut input);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The default arrangement at an ordinary window size.
    ///
    /// Ignored because it is for looking at, not for asserting: a golden-image comparison would
    /// fail on every font and driver difference and teach everyone to ignore it.
    #[test]
    #[ignore = "writes a PNG to look at rather than asserting anything"]
    fn shot_default_workspace() {
        shoot(
            "default-1400x900",
            Rect::new(0.0, 0.0, 1400.0, 900.0),
            &[],
            &mut plain_workspace(),
        );
    }

    /// The same arrangement in a small window, where every strip is under pressure.
    #[test]
    #[ignore = "writes a PNG to look at rather than asserting anything"]
    fn shot_small_window() {
        shoot(
            "small-900x600",
            Rect::new(0.0, 0.0, 900.0, 600.0),
            &[],
            &mut plain_workspace(),
        );
    }

    /// The menu squeezed until `Direction::Auto` turns it into a column.
    ///
    /// Set by weight rather than by simulating a drag: the flip depends on the width the panel
    /// ends up with, and nothing about it is made more real by pretending a pointer did it.
    #[test]
    #[ignore = "writes a PNG to look at rather than asserting anything"]
    fn shot_narrow_menu() {
        let mut ws = plain_workspace();
        ws.layout.set_weight(&[0], 0.5);
        ws.layout.set_weight(&[1], 0.5);
        shoot(
            "narrow-menu",
            Rect::new(0.0, 0.0, 420.0, 900.0),
            &[],
            &mut ws,
        );
    }

    /// Every icon in every set, side by side, at the size they will actually be drawn.
    ///
    /// The only way to know whether a glyph reads at six millimetres is to look at it at six
    /// millimetres. Drawn twice on each row: once at tool-rail size, once magnified, so a shape
    /// that is merely *wrong* can be told apart from one that is merely small.
    #[test]
    #[ignore = "writes a PNG to look at rather than asserting anything"]
    fn shot_icons() {
        use crate::icons::{Symbol, SETS};
        let theme = Theme::default();
        let m = theme.metrics;
        let drawn: Vec<&crate::icons::IconSet> = SETS
            .iter()
            .filter(|s| s.glyph(Symbol::Brush).is_some())
            .collect();
        let big = 64.0;
        let col = big + m.row + m.padding * 4.0;
        let w = 200.0 + col * drawn.len() as f32;
        let h = m.padding * 2.0 + Symbol::ALL.len() as f32 * (big + m.gap);

        let Some((device, queue)) = crate::test_gpu::try_device() else {
            eprintln!("screenshot icons: no GPU adapter, skipped");
            return;
        };
        let screen = Rect::new(0.0, 0.0, w, h);
        let ctx = egui::Context::default();
        let mut passes = run_frames(&ctx, screen, &[], |c| {
            let painter = c.layer_painter(egui::LayerId::new(
                egui::Order::Background,
                egui::Id::new("icons"),
            ));
            painter.rect_filled(
                egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(w, h)),
                0.0,
                egui::Color32::from_rgb(
                    theme.palette.panel.0[0],
                    theme.palette.panel.0[1],
                    theme.palette.panel.0[2],
                ),
            );
            let text = egui::Color32::from_rgb(
                theme.palette.text.0[0],
                theme.palette.text.0[1],
                theme.palette.text.0[2],
            );
            for (row, symbol) in Symbol::ALL.iter().enumerate() {
                let y = m.padding + row as f32 * (big + m.gap);
                painter.text(
                    egui::pos2(m.padding, y + big / 2.0),
                    egui::Align2::LEFT_CENTER,
                    format!("{symbol:?}"),
                    egui::FontId::proportional(m.body),
                    egui::Color32::from_rgb(
                        theme.palette.dim.0[0],
                        theme.palette.dim.0[1],
                        theme.palette.dim.0[2],
                    ),
                );
                for (i, set) in drawn.iter().enumerate() {
                    let Some(marks) = set.glyph(*symbol) else {
                        continue;
                    };
                    let x = 200.0 + i as f32 * col;
                    // At the size a tool button actually gives it.
                    crate::panel_draw::draw_icon(
                        &painter,
                        marks,
                        Rect::new(x, y + (big - m.row) / 2.0, m.row, m.row),
                        text,
                    );
                    // And magnified, to tell a bad shape from a small one.
                    crate::panel_draw::draw_icon(
                        &painter,
                        marks,
                        Rect::new(x + m.row + m.padding, y, big, big),
                        text,
                    );
                }
            }
            for (i, set) in drawn.iter().enumerate() {
                painter.text(
                    egui::pos2(200.0 + i as f32 * col, 2.0),
                    egui::Align2::LEFT_TOP,
                    set.name,
                    egui::FontId::proportional(m.label),
                    text,
                );
            }
        });
        let mut renderer =
            egui_wgpu::Renderer::new(device, crate::test_gpu::SURFACE, None, 1, true);
        for pass in &passes {
            for (id, delta) in &pass.textures_delta.set {
                renderer.update_texture(device, queue, *id, delta);
            }
        }
        let output = passes.pop().expect("a pass");
        render_to_png("icons", screen, device, queue, &mut renderer, &ctx, output);
    }

    /// All three colour wheels, at the size a docked Colour panel gives them.
    ///
    /// The geometry is proved without a screen; what a screenshot answers is whether the drawing
    /// agrees with it -- a marker in the wrong place, a ring drawn as a disc, a triangle that does
    /// not turn with its hue.
    #[test]
    #[ignore = "writes a PNG to look at rather than asserting anything"]
    fn shot_colour_wheels() {
        use crate::colour_wheel::{Hsv, Shape};
        let theme = Theme::default();
        let side = 190.0;
        let gap = 24.0;
        let shapes = [Shape::Ring, Shape::Triangle, Shape::Square];
        let colours = [
            Hsv::new(20.0, 0.9, 0.95),
            Hsv::new(150.0, 0.6, 0.7),
            Hsv::new(260.0, 0.35, 0.5),
        ];
        let screen = Rect::new(
            0.0,
            0.0,
            gap + (side + gap) * shapes.len() as f32,
            gap + (side + gap) * colours.len() as f32,
        );

        let Some((device, queue)) = crate::test_gpu::try_device() else {
            eprintln!("screenshot colour-wheels: no GPU adapter, skipped");
            return;
        };
        let ctx = egui::Context::default();
        let mut passes = run_frames(&ctx, screen, &[], |c| {
            let painter = c.layer_painter(egui::LayerId::new(
                egui::Order::Background,
                egui::Id::new("wheels"),
            ));
            painter.rect_filled(
                egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(screen.w, screen.h)),
                0.0,
                egui::Color32::from_rgb(
                    theme.palette.panel.0[0],
                    theme.palette.panel.0[1],
                    theme.palette.panel.0[2],
                ),
            );
            for (row, colour) in colours.iter().enumerate() {
                for (col, shape) in shapes.iter().enumerate() {
                    let at = Rect::new(
                        gap + (side + gap) * col as f32,
                        gap + (side + gap) * row as f32,
                        side,
                        side,
                    );
                    let mut holding = None;
                    crate::panel_draw::draw_wheel(
                        &painter,
                        &theme,
                        crate::panel_draw::WheelAt {
                            within: at,
                            shape: *shape,
                            colour: *colour,
                        },
                        &crate::panel_draw::PanelInput::default(),
                        &mut holding,
                    );
                }
            }
        });
        let mut renderer =
            egui_wgpu::Renderer::new(device, crate::test_gpu::SURFACE, None, 1, true);
        for pass in &passes {
            for (id, delta) in &pass.textures_delta.set {
                renderer.update_texture(device, queue, *id, delta);
            }
        }
        let output = passes.pop().expect("a pass");
        render_to_png(
            "colour-wheels",
            screen,
            device,
            queue,
            &mut renderer,
            &ctx,
            output,
        );
    }

    /// A panel lifted out of the arrangement, floating above it.
    #[test]
    #[ignore = "writes a PNG to look at rather than asserting anything"]
    fn shot_floating() {
        let mut ws = plain_workspace();
        ws.float(crate::workspace::TOOL);
        ws.float(crate::workspace::COLOUR);
        shoot("floating", Rect::new(0.0, 0.0, 1400.0, 900.0), &[], &mut ws);
    }

    /// A panel in the air, waiting for the artist to say where it goes.
    ///
    /// Worth looking at rather than only asserting: the whole of this feature is that the
    /// arrangement itself is the menu, so what it looks like *is* the interface.
    #[test]
    #[ignore = "writes a PNG to look at rather than asserting anything"]
    fn shot_placing() {
        let screen = Rect::new(0.0, 0.0, 1400.0, 900.0);
        let mut ws = plain_workspace();
        ws.float(crate::workspace::COLOUR);
        // Taken out, and the pointer resting where it would land: over the left band of the
        // canvas, which would split it.
        ws.start_placing(crate::workspace::COLOUR);
        shoot("placing", screen, &[Poke::Move(420.0, 500.0)], &mut ws);
    }

    /// **Nothing in a panel answers the press that puts a panel down.**
    ///
    /// A pick is answered by pressing wherever the panel should go, and that is as often as not on
    /// top of a control -- so without this the artist puts a panel down and changes the brush size
    /// at the same time.
    ///
    /// **The case chosen is the one that is not saved by ordering.** A press that lands somewhere
    /// rearranges the leaf it lands in, so the control under it has usually moved by the time egui
    /// looks -- which is luck, not a rule, and it does not hold when the press lands *nowhere*: the
    /// pick is put back, the arrangement is exactly as it was, and the control is still sitting
    /// under the pointer. Here that is a floating window offered a panel that may not float.
    ///
    /// Driven through a real egui frame rather than by reading a flag, because what is being
    /// checked is whether egui hands the press to a widget, which no amount of asserting about
    /// `Workspace` can see -- and through `ws.show`, so it is the application's own draw path.
    ///
    /// Three things stop that press between them and this cannot tell them apart: the panels are
    /// disabled, the landing rearranges the leaf, and the overlay's layer occludes what is beneath
    /// it. That is fine here -- the promise is that no control answers, not which of the three
    /// keeps it -- but it does mean removing any one of them on its own leaves this green.
    #[test]
    fn a_panel_answers_nothing_while_a_pick_is_waiting() {
        for waiting in [false, true] {
            let screen = Rect::new(0.0, 0.0, 1400.0, 900.0);
            let mut ws = plain_workspace();
            ws.set_screen(screen);
            ws.float(crate::workspace::TOOL);
            if waiting {
                // The canvas may not float, so the window below is no destination for it: the
                // press will put the pick back and change nothing.
                ws.start_placing(crate::workspace::CANVAS);
            }

            let at = control_point(&ws, crate::workspace::TOOL, screen);
            let mut changed = Vec::new();
            let ctx = egui::Context::default();
            let _ = run_frames(
                &ctx,
                screen,
                &[
                    Poke::Move(at.0, at.1),
                    Poke::Press(at.0, at.1),
                    Poke::Release(at.0, at.1),
                ],
                |c| {
                    changed.clear();
                    ws.show(
                        c,
                        screen,
                        crate::workspace::Attention::Workspace,
                        |panel, ui, direction, place| {
                            if panel == crate::workspace::TOOL {
                                let theme = Theme::default();
                                let mut input = crate::panel_draw::PanelInput::default();
                                changed = crate::panel_draw::show(
                                    ui,
                                    &[Control::Slider {
                                        id: 0,
                                        text: "Size".to_owned(),
                                        value: 14.0,
                                        min: 0.5,
                                        max: 400.0,
                                        unit: "px",
                                        log: true,
                                    }],
                                    &theme,
                                    direction,
                                    &mut input,
                                );
                            } else {
                                filler(panel, ui, direction, place);
                            }
                        },
                    );
                },
            );

            if waiting {
                assert!(
                    changed.is_empty(),
                    "a control answered the press that was answering a pick: {changed:?}"
                );
            } else {
                // The other half: without a pick the very same press *does* reach the control.
                // Otherwise this would pass just as well on a panel nobody can press at all.
                assert!(
                    !changed.is_empty(),
                    "the press never reached the control, so the other half proves nothing"
                );
            }
        }
    }

    /// The middle of a panel's first row of controls, in screen units.
    fn control_point(ws: &Workspace, panel: crate::layout::PanelId, screen: Rect) -> (f32, f32) {
        let area = ws
            .content_of(panel, screen)
            .expect("the panel is somewhere");
        let m = Theme::default().metrics;
        (area.x + area.w / 2.0, area.y + m.row / 2.0)
    }

    /// A floating window over a docked panel, to see that it is in front of it.
    #[test]
    #[ignore = "writes a PNG to look at rather than asserting anything"]
    fn shot_floating_in_front() {
        let screen = Rect::new(0.0, 0.0, 1400.0, 900.0);
        let mut ws = plain_workspace();
        ws.set_screen(screen);
        ws.float(crate::workspace::COLOUR);
        // Squarely over the panels down the right-hand side, which draw content of their own.
        ws.put_window_for_test(0, Rect::new(1000.0, 120.0, 320.0, 300.0));
        shoot("floating-in-front", screen, &[], &mut ws);
    }

    use crate::panel_ui::Change;

    /// **A text field takes the caret, takes what is typed, and reports it once.**
    ///
    /// End to end through a real egui frame, because everything that could be wrong about a field
    /// is in the wiring: whether the press gives it the caret, whether the keys reach it at all,
    /// and whether what comes back out is what was typed.
    #[test]
    fn a_text_field_reports_what_was_typed_when_it_is_finished_with() {
        let area = Rect::new(0.0, 0.0, 300.0, 120.0);
        let field = || Control::Text {
            id: 7,
            text: "Name".to_owned(),
            value: "Layer 1".to_owned(),
        };
        let m = Theme::default().metrics;
        let at = (area.x + area.w * 0.75, area.y + m.row / 2.0);

        // Pressing it takes the caret and says so, and nothing has changed yet.
        let (changes, input) = type_into(&[field()], area, &[tap(at)]);
        assert_eq!(
            changes,
            vec![Change::Typing(7)],
            "the press did not take the caret"
        );
        assert!(input.editing.is_some(), "and nothing is being edited");

        // Typing replaces what was there -- the field arrives selected, as every field does -- and
        // Enter finishes it.
        let (changes, input) = type_into(
            &[field()],
            area,
            &[tap(at), vec![Poke::Say("Sky"), Poke::Tap(egui::Key::Enter)]],
        );
        assert!(
            changes.contains(&Change::Typed(7, "Sky".to_owned())),
            "the field did not report what was typed: {changes:?}"
        );
        assert!(
            input.editing.is_none(),
            "and it should have let go of the caret"
        );
    }

    /// Backspace and the arrows reach the field, and nothing is reported until it is finished.
    #[test]
    fn a_text_field_edits_before_it_reports() {
        let area = Rect::new(0.0, 0.0, 300.0, 120.0);
        let m = Theme::default().metrics;
        let at = (area.x + area.w * 0.75, area.y + m.row / 2.0);
        let one = [Control::Text {
            id: 3,
            text: String::new(),
            value: "ab".to_owned(),
        }];

        // Typed but not finished: the words are in the field and nobody has been told.
        let (changes, input) = type_into(&one, area, &[tap(at), vec![Poke::Say("xy")]]);
        assert!(
            !changes.iter().any(|c| matches!(c, Change::Typed(..))),
            "it reported before it was finished with: {changes:?}"
        );
        assert_eq!(input.editing.as_ref().map(|(_, f)| f.text()), Some("xy"));

        // Backspace takes one off, and the caret keys move rather than type.
        let (_, input) = type_into(
            &one,
            area,
            &[
                tap(at),
                vec![
                    Poke::Say("xyz"),
                    Poke::Tap(egui::Key::Backspace),
                    Poke::Tap(egui::Key::ArrowLeft),
                    Poke::Say("-"),
                ],
            ],
        );
        assert_eq!(input.editing.as_ref().map(|(_, f)| f.text()), Some("x-y"));
    }

    /// **Pressing something else finishes the field**, rather than letting it eat the next thing.
    #[test]
    fn pressing_elsewhere_finishes_a_field() {
        let area = Rect::new(0.0, 0.0, 300.0, 160.0);
        let m = Theme::default().metrics;
        let controls = [
            Control::Text {
                id: 3,
                text: String::new(),
                value: "old".to_owned(),
            },
            Control::Button {
                id: 4,
                text: "Apply".to_owned(),
            },
        ];
        let on_field = (area.x + area.w * 0.75, area.y + m.row / 2.0);
        let on_button = (area.x + area.w / 2.0, area.y + m.row * 1.5);

        let (changes, input) = type_into(
            &controls,
            area,
            &[tap(on_field), vec![Poke::Say("new")], tap(on_button)],
        );
        assert!(
            changes.contains(&Change::Typed(3, "new".to_owned())),
            "the field did not report when the pointer went elsewhere: {changes:?}"
        );
        assert!(input.editing.is_none());
    }

    /// **Which button, and the moment it went down** -- both, and only over this panel.
    ///
    /// `pressed` says a button is *held*, which is what a wheel wants. A palette wants to know a
    /// click happened and a delete wants to know it was the other button: without both, a
    /// right-click held over a swatch forgets a colour every frame, and a right-drag across the
    /// palette empties it. Two panels reached around this into raw egui for want of it.
    #[test]
    fn a_custom_control_is_told_which_button_and_when() {
        let area = Rect::new(0.0, 0.0, 200.0, 120.0);
        let one = [Control::Custom {
            id: 5,
            height: 80.0,
        }];
        let inside = (area.x + area.w / 2.0, area.y + 40.0);

        // Nothing pressed: neither edge.
        let (_, quiet) = type_into(&one, area, &[vec![Poke::Move(inside.0, inside.1)]]);
        assert!(!quiet.clicked && !quiet.other_clicked);

        // The main button going down is an edge, and it is not the other one.
        let (_, tapped) = type_into(
            &one,
            area,
            &[vec![
                Poke::Move(inside.0, inside.1),
                Poke::Press(inside.0, inside.1),
            ]],
        );
        assert!(tapped.clicked, "a press over the panel was not reported");
        assert!(!tapped.other_clicked, "and it was not the other button");

        // **Held is not clicked.** The frame after the press reports the button still down and no
        // new edge -- which is the whole difference, and the bug the two panels were working
        // around.
        let (_, held) = type_into(
            &one,
            area,
            &[
                vec![
                    Poke::Move(inside.0, inside.1),
                    Poke::Press(inside.0, inside.1),
                ],
                vec![Poke::Move(inside.0 + 1.0, inside.1)],
            ],
        );
        assert!(held.pressed, "the button should still be down");
        assert!(!held.clicked, "holding it is not clicking it again");

        // And a press with the pointer off the panel is not this panel's.
        let outside = (area.x + area.w + 20.0, area.y + area.h + 20.0);
        let (_, elsewhere) = type_into(
            &one,
            area,
            &[vec![
                Poke::Move(outside.0, outside.1),
                Poke::Press(outside.0, outside.1),
            ]],
        );
        assert!(
            !elsewhere.clicked,
            "a press elsewhere was taken as this panel's"
        );
    }

    /// **A caption typed into the panel comes back out of it.**
    ///
    /// The one line that makes the whole caption editor work: `show` folding the frame's changes
    /// into its copy of the block. Everything either side of it is proved -- the editor by its own
    /// tests, the writing back by `written_back` -- and between them sat a transcription that
    /// could be deleted with the suite still green.
    ///
    /// So this drives the real panel through a real frame: a text layer under it, a press on the
    /// caption field, words typed, Enter. What comes out must be the words.
    #[test]
    fn a_caption_typed_into_the_panel_reaches_the_shell() {
        use crate::ui::Picked;
        let screen = Rect::new(0.0, 0.0, 400.0, 600.0);
        let area = Rect::new(0.0, 0.0, 360.0, 560.0);
        let layers = vec![openpaint_core::Layer::restored(
            0,
            "Caption",
            1.0,
            openpaint_core::Blend::Normal,
            true,
            false,
            false,
        )
        .with_text(openpaint_core::TextBlock {
            text: "Before".to_owned(),
            ..openpaint_core::TextBlock::default()
        })];

        let theme = Theme::default();
        let mut brush = openpaint_core::Brush::default();
        let mut colour = brush.color_srgb8();
        let mut input = crate::panel_draw::PanelInput::default();
        let mut menu = None;
        let mut pick = None;
        let mut shape = crate::colour_wheel::Shape::default();
        let mut hold = None;
        let mut answers: Vec<Picked> = Vec::new();

        // Where the caption field is: the first `Text` control the panel offers.
        let ctx = egui::Context::default();
        let mut at = (0.0, 0.0);
        for pass in 0..3 {
            let mut raw = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::pos2(screen.x, screen.y),
                    egui::vec2(screen.w, screen.h),
                )),
                ..Default::default()
            };
            // Pass 1 takes the caret, pass 2 types and finishes. One batch per frame, because a
            // press and a release are one click and there is one click per frame.
            if pass == 1 {
                for poke in [
                    Poke::Move(at.0, at.1),
                    Poke::Press(at.0, at.1),
                    Poke::Release(at.0, at.1),
                ] {
                    raw.events.push(as_event(poke));
                }
            } else if pass == 2 {
                for poke in [Poke::Say("After"), Poke::Tap(egui::Key::Enter)] {
                    raw.events.push(as_event(poke));
                }
            }
            let _ = ctx.run(raw, |c| {
                let status = crate::ui::Status {
                    history: (0, 0, 0),
                    message: None,
                    page_size: (100, 100),
                    crop: None,
                    crop_rect: None,
                    residency: (0, 0),
                    spilled: 0,
                    traffic: (0, 0),
                    layers: &layers,
                    active_layer: 0,
                    pages: (1, 0),
                    tool: crate::editor::Tool::Brush,
                    confirm: None,
                    brush_cursor: None,
                    perf: crate::perf::PerfSnapshot::default(),
                    recovery: None,
                    palette: &[],
                    presets: &[],
                    preset_trouble: None,
                    font_families: &[],
                    font_substituted: None,
                    transform: None,
                    transform_box: None,
                    kernel: openpaint_core::Kernel::default(),
                    autosave: "",
                    export: None,
                    selection: &[],
                    select_tool: None,
                    has_selection: false,
                    wand: crate::ui::WandSettings::default(),
                };
                let mut ui = egui::Ui::new(
                    c.clone(),
                    egui::LayerId::new(egui::Order::Middle, egui::Id::new("caption")),
                    egui::Id::new("caption-ui"),
                    egui::UiBuilder::new().max_rect(egui::Rect::from_min_size(
                        egui::pos2(area.x, area.y),
                        egui::vec2(area.w, area.h),
                    )),
                );
                let mut paint = crate::ui::Painting {
                    theme: &theme,
                    direction: Direction::Column,
                    input: &mut input,
                    menu: &mut menu,
                    pick: &mut pick,
                    extend_by: 512,
                    preset_name: "",
                    ctx: c,
                    wheel_shape: &mut shape,
                    wheel_hold: &mut hold,
                };
                if let Some(what) = crate::panels::show(
                    crate::workspace::TEXT,
                    &mut ui,
                    &mut brush,
                    &mut colour,
                    &status,
                    &mut paint,
                    crate::workspace::Place::Panel,
                ) {
                    answers.push(what);
                }
            });
            // After the first pass the controls have been laid out, so the field can be found.
            if pass == 0 {
                let m = theme.metrics;
                let mut y = area.y;
                for control in crate::panels::text::controls_for_test(&layers[0]) {
                    let tall = crate::panel_draw::wrapped_height(&ctx, m.body, &control, area.w);
                    let h = control.height(&m, tall);
                    if matches!(control, Control::Text { .. }) {
                        at = (area.x + area.w * 0.75, y + h / 2.0);
                        break;
                    }
                    y += h + m.gap;
                }
            }
        }

        assert!(
            answers.iter().any(|a| matches!(
                a,
                Picked::TextSet(block) if block.text == "After"
            )),
            "the caption never reached the shell: {answers:?}"
        );
    }

    /// A pick answers by asking to be opened. It chooses nothing by itself.
    #[test]
    fn a_pick_asks_to_be_opened() {
        let area = Rect::new(0.0, 0.0, 300.0, 120.0);
        let m = Theme::default().metrics;
        let at = (area.x + area.w / 2.0, area.y + m.row / 2.0);
        let (changes, _) = press_controls(
            &[Control::Pick {
                id: 9,
                text: "Blend".to_owned(),
                value: "Multiply".to_owned(),
            }],
            area,
            Direction::Column,
            at,
        );
        assert_eq!(changes, vec![Change::Pressed(9)]);
    }

    /// A pointer arriving, pressing and letting go, which is what a tap is made of.
    fn tap(at: (f32, f32)) -> Vec<Poke> {
        vec![
            Poke::Move(at.0, at.1),
            Poke::Press(at.0, at.1),
            Poke::Release(at.0, at.1),
        ]
    }

    /// Drive a list of controls through a run of real frames and hand back everything that
    /// changed and the state the engine kept.
    ///
    /// **One batch of events per frame, because that is what a frame is.** Two taps delivered
    /// together are one tap as far as egui is concerned -- a press and a release coalesce into a
    /// click and there is one click per frame -- so "press the field, then press something else"
    /// cannot be said at all in a single batch, and a test that tried it read as passing.
    fn type_into(
        controls: &[Control],
        area: Rect,
        frames: &[Vec<Poke>],
    ) -> (Vec<crate::panel_ui::Change>, crate::panel_draw::PanelInput) {
        let theme = Theme::default();
        let mut input = crate::panel_draw::PanelInput::default();
        let mut changes = Vec::new();
        let screen = Rect::new(0.0, 0.0, area.x + area.w + 40.0, area.y + area.h + 40.0);
        let rect = egui::Rect::from_min_size(
            egui::pos2(screen.x, screen.y),
            egui::vec2(screen.w, screen.h),
        );
        let ctx = egui::Context::default();
        // A warm-up pass first, then one per batch: egui measures text from an atlas it builds as
        // it goes, so the first frame after a context is made can lay everything out at zero.
        for pass in 0..=frames.len() {
            let mut raw = egui::RawInput {
                screen_rect: Some(rect),
                ..Default::default()
            };
            if pass > 0 {
                for poke in &frames[pass - 1] {
                    raw.events.push(as_event(*poke));
                }
            }
            let _ = ctx.run(raw, |c| {
                let mut ui = egui::Ui::new(
                    c.clone(),
                    egui::LayerId::new(egui::Order::Middle, egui::Id::new("typed")),
                    egui::Id::new("typed-ui"),
                    egui::UiBuilder::new().max_rect(egui::Rect::from_min_size(
                        egui::pos2(area.x, area.y),
                        egui::vec2(area.w, area.h),
                    )),
                );
                let out = crate::panel_draw::show(
                    &mut ui,
                    controls,
                    &theme,
                    Direction::Column,
                    &mut input,
                );
                if pass > 0 {
                    changes.extend(out);
                }
            });
        }
        (changes, input)
    }

    /// Every ported panel at once, floated so each is visible at a usable size.
    ///
    /// Nine panels were written by nine people against one written specification. This is the only
    /// thing that says whether they look like one application -- and three of them draw things no
    /// assertion can see: a curve, a palette grid, a caption.
    ///
    /// **Drawn by the panels themselves**, not by `filler`. A picture of stand-in controls under
    /// the ported panels' names would say nothing about the ported panels, and would say it
    /// convincingly.
    #[test]
    #[ignore = "writes a PNG to look at rather than asserting anything"]
    fn shot_ported_panels() {
        use crate::workspace as w;
        let screen = Rect::new(0.0, 0.0, 1800.0, 1000.0);
        let mut ws = plain_workspace();
        ws.set_screen(screen);
        for (i, panel) in [w::TRANSFORM, w::PAGES, w::PAGE, w::SELECT, w::TEXT]
            .into_iter()
            .enumerate()
        {
            ws.float(panel);
            let (col, row) = (i % 3, i / 3);
            ws.put_window_for_test(
                i,
                Rect::new(
                    40.0 + 340.0 * col as f32,
                    40.0 + 470.0 * row as f32,
                    320.0,
                    450.0,
                ),
            );
        }
        shoot_panels("ported-panels", screen, &mut ws);
    }

    /// The brush panel, which is the one with the drawings in it.
    #[test]
    #[ignore = "writes a PNG to look at rather than asserting anything"]
    fn shot_brush_panel() {
        let screen = Rect::new(0.0, 0.0, 1400.0, 1000.0);
        let mut ws = plain_workspace();
        ws.set_screen(screen);
        ws.float(crate::workspace::TOOL);
        ws.put_window_for_test(0, Rect::new(60.0, 40.0, 360.0, 920.0));
        shoot_panels("brush-panel", screen, &mut ws);
    }

    /// The layers and colour panels, which have a list and a grid in them.
    #[test]
    #[ignore = "writes a PNG to look at rather than asserting anything"]
    fn shot_layers_and_colour() {
        let screen = Rect::new(0.0, 0.0, 1000.0, 800.0);
        let mut ws = plain_workspace();
        ws.set_screen(screen);
        ws.float(crate::workspace::LAYERS);
        ws.put_window_for_test(0, Rect::new(50.0, 40.0, 330.0, 700.0));
        ws.float(crate::workspace::COLOUR);
        ws.put_window_for_test(1, Rect::new(420.0, 40.0, 330.0, 700.0));
        shoot_panels("layers-and-colour", screen, &mut ws);
    }

    /// **A drag that wanders off the hue ring keeps setting the hue.**
    ///
    /// The same latch a slider has, and for the same reason: a hand sweeping round a ring does not
    /// stay inside a twenty-unit band, and a picker that let go the moment it strayed would be
    /// unusable. Without it the colour would jump to whatever happened to be under the pointer,
    /// which for a point outside the wheel is nothing at all.
    #[test]
    fn a_drag_off_the_hue_ring_keeps_setting_the_hue() {
        use crate::colour_wheel::{Hsv, Region, Shape};
        use crate::panel_draw::{draw_wheel, PanelInput, WheelAt};
        let theme = Theme::default();
        let within = Rect::new(0.0, 0.0, 200.0, 200.0);
        let start = Hsv::new(0.0, 1.0, 1.0);

        // The top of the ring, which is where red lives.
        let on_ring = (within.x + within.w / 2.0, within.y + 6.0);
        let mut holding = None;
        let picked = frame_with(Rect::new(0.0, 0.0, 400.0, 400.0), &[], |ctx| {
            let painter = ctx.layer_painter(egui::LayerId::new(
                egui::Order::Background,
                egui::Id::new("wheel-test"),
            ));
            let mut input = PanelInput {
                pointer: Some(on_ring),
                pressed: true,
                ..PanelInput::default()
            };
            input.pointer = Some(on_ring);
            draw_wheel(
                &painter,
                &theme,
                WheelAt {
                    within,
                    shape: Shape::Ring,
                    colour: start,
                },
                &input,
                &mut holding,
            )
        });
        assert!(picked.is_some(), "a press on the ring should set a colour");
        assert_eq!(holding, Some(Region::Hue), "and it should have taken hold");

        // Now well outside the wheel, still pressed.
        let strayed = frame_with(Rect::new(0.0, 0.0, 400.0, 400.0), &[], |ctx| {
            let painter = ctx.layer_painter(egui::LayerId::new(
                egui::Order::Background,
                egui::Id::new("wheel-test-2"),
            ));
            let input = PanelInput {
                pointer: Some((350.0, 350.0)),
                pressed: true,
                ..PanelInput::default()
            };
            draw_wheel(
                &painter,
                &theme,
                WheelAt {
                    within,
                    shape: Shape::Ring,
                    colour: start,
                },
                &input,
                &mut holding,
            )
        });
        assert!(
            strayed.is_some(),
            "a drag off the ring should keep setting the hue, not stop"
        );
        assert_eq!(holding, Some(Region::Hue), "and keep hold of the ring");

        // Letting go releases it, so the next press can land somewhere else.
        frame_with(Rect::new(0.0, 0.0, 400.0, 400.0), &[], |ctx| {
            let painter = ctx.layer_painter(egui::LayerId::new(
                egui::Order::Background,
                egui::Id::new("wheel-test-3"),
            ));
            let input = PanelInput {
                pointer: Some(on_ring),
                pressed: false,
                ..PanelInput::default()
            };
            draw_wheel(
                &painter,
                &theme,
                WheelAt {
                    within,
                    shape: Shape::Ring,
                    colour: start,
                },
                &input,
                &mut holding,
            )
        });
        assert_eq!(holding, None, "letting go should release the ring");
    }

    /// A wheel with no pointer over it sets nothing, and lets go of whatever it held.
    #[test]
    fn a_wheel_with_no_pointer_sets_nothing() {
        use crate::colour_wheel::{Hsv, Region, Shape};
        use crate::panel_draw::{draw_wheel, PanelInput, WheelAt};
        let theme = Theme::default();
        let mut holding = Some(Region::Hue);
        let picked = frame_with(Rect::new(0.0, 0.0, 400.0, 400.0), &[], |ctx| {
            let painter = ctx.layer_painter(egui::LayerId::new(
                egui::Order::Background,
                egui::Id::new("wheel-none"),
            ));
            draw_wheel(
                &painter,
                &theme,
                WheelAt {
                    within: Rect::new(0.0, 0.0, 200.0, 200.0),
                    shape: Shape::Ring,
                    colour: Hsv::new(0.0, 1.0, 1.0),
                },
                &PanelInput {
                    pointer: None,
                    pressed: true,
                    ..PanelInput::default()
                },
                &mut holding,
            )
        });
        assert_eq!(picked, None, "a pointer that is not there set a colour");
        assert_eq!(holding, None, "and it should have let go");
    }

    /// **A custom control is handed the rectangle it was given**, or nothing can draw into it.
    ///
    /// The engine decides where it goes; a panel that worked the position out again would be a
    /// second answer to drift from the first, and the symptom is a colour wheel drawn somewhere
    /// other than where pressing it reads.
    #[test]
    fn a_custom_control_is_told_where_it_landed() {
        let controls = vec![
            Control::Label {
                text: "Colour".to_owned(),
            },
            Control::Custom {
                id: 9,
                height: 120.0,
            },
        ];
        let area = Rect::new(20.0, 20.0, 220.0, 300.0);
        let (_, input) = press_controls(&controls, area, Direction::Column, (0.0, 0.0));
        let (id, at) = *input
            .custom
            .first()
            .expect("the custom control's rectangle was never reported");
        assert_eq!(id, 9);
        assert!(
            (at.h - 120.0).abs() < 0.001,
            "it asked for 120 and was given {}",
            at.h
        );
        // And it is where `place` put it, not at the corner of the panel: under the label, by
        // however tall that label's words actually are. Measured the same way the layout measures
        // it -- a label's height is its content now, so a second guess here would be a second
        // answer to the same question.
        let m = Theme::default().metrics;
        let label_tall = frame_with(area, &[], |ctx| {
            crate::panel_draw::wrapped_height(ctx, m.body, &controls[0], area.w)
        });
        let expected = area.y + controls[0].height(&m, label_tall) + m.gap;
        assert!(
            (at.y - expected).abs() < 0.001,
            "it landed at {} rather than {expected}",
            at.y
        );
    }

    /// **A press on a control produces its change, all the way through egui.**
    ///
    /// Every other test of this stops at `panel_ui`, which says what a press at a point *should*
    /// mean. This one presses.
    #[test]
    fn pressing_a_button_in_a_real_frame_reports_it() {
        let controls = vec![
            Control::Label {
                text: "Layers".to_owned(),
            },
            Control::Button {
                id: 42,
                text: "Merge down".to_owned(),
            },
        ];
        let area = Rect::new(20.0, 20.0, 220.0, 300.0);
        let m = Theme::default().metrics;
        // The button is the second control, so it sits one label plus one gap down.
        let y = area.y + controls[0].height(&m, 0.0) + m.gap + m.row / 2.0;
        let (changes, input) =
            press_controls(&controls, area, Direction::Column, (area.x + 60.0, y));
        assert!(
            changes.contains(&crate::panel_ui::Change::Pressed(42)),
            "pressing the button reported {changes:?}"
        );
        assert!(
            input.pressed_rect.is_some(),
            "and where it was pressed, or a menu has nothing to open under"
        );
    }

    /// A press on nothing reports nothing, and does not invent a place to anchor to.
    #[test]
    fn pressing_empty_space_reports_nothing() {
        let controls = vec![Control::Button {
            id: 42,
            text: "Merge down".to_owned(),
        }];
        let area = Rect::new(20.0, 20.0, 220.0, 300.0);
        let (changes, input) = press_controls(
            &controls,
            area,
            Direction::Column,
            (area.x + 60.0, area.y + 250.0),
        );
        assert!(
            changes.is_empty(),
            "a press on nothing reported {changes:?}"
        );
        assert!(input.pressed_rect.is_none());
    }
}
