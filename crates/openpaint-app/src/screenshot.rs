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
                input.events.push(match *poke {
                    Poke::Move(x, y) => egui::Event::PointerMoved(egui::pos2(x, y)),
                    Poke::Press(x, y) | Poke::Release(x, y) => egui::Event::PointerButton {
                        pos: egui::pos2(x, y),
                        button: egui::PointerButton::Primary,
                        pressed: matches!(poke, Poke::Press(..)),
                        modifiers: egui::Modifiers::default(),
                    },
                });
            }
        }
        passes.push(ctx.run(input, &mut build));
    }
    passes
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
        ws.show(c, screen, |panel, ui, direction, place| {
            filler(panel, ui, direction, place);
        });
    });

    let mut renderer = egui_wgpu::Renderer::new(&device, crate::test_gpu::SURFACE, None, 1, true);
    // Every pass's textures, in order: the font atlas arrives with the first one and never again.
    for pass in &passes {
        for (id, delta) in &pass.textures_delta.set {
            renderer.update_texture(&device, &queue, *id, delta);
        }
    }
    let output = passes.pop().expect("at least one pass");
    render_to_png(name, screen, &device, &queue, &mut renderer, &ctx, output);
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
            egui_wgpu::Renderer::new(&device, crate::test_gpu::SURFACE, None, 1, true);
        for pass in &passes {
            for (id, delta) in &pass.textures_delta.set {
                renderer.update_texture(&device, &queue, *id, delta);
            }
        }
        let output = passes.pop().expect("a pass");
        render_to_png(
            "icons",
            screen,
            &device,
            &queue,
            &mut renderer,
            &ctx,
            output,
        );
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
            egui_wgpu::Renderer::new(&device, crate::test_gpu::SURFACE, None, 1, true);
        for pass in &passes {
            for (id, delta) in &pass.textures_delta.set {
                renderer.update_texture(&device, &queue, *id, delta);
            }
        }
        let output = passes.pop().expect("a pass");
        render_to_png(
            "colour-wheels",
            screen,
            &device,
            &queue,
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
        ws.float(crate::workspace::BRUSH);
        ws.float(crate::workspace::COLOUR);
        shoot("floating", Rect::new(0.0, 0.0, 1400.0, 900.0), &[], &mut ws);
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
        // And it is where `place` put it, not at the corner of the panel.
        let m = Theme::default().metrics;
        let expected = area.y + controls[0].height(&m) + m.gap;
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
        let y = area.y + controls[0].height(&m) + m.gap + m.row / 2.0;
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
