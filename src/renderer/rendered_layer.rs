use itertools::Itertools;
use skia_safe::{
    canvas::SaveLayerRec,
    image_filters::blur,
    utils::shadow_utils::{draw_shadow, ShadowFlags},
    BlendMode, Canvas, ClipOp, Color, Paint, Path, PathOp, Point3, RRect, Rect,
};

use glamour::Intersection;

use crate::{
    editor::WindowType,
    units::{to_skia_rect, GridScale, PixelRect},
};

use super::{RenderedWindow, RendererSettings, WindowDrawDetails};

struct LayerWindow<'w> {
    window: &'w mut RenderedWindow,
    group: usize,
}

pub struct FloatingLayer<'w> {
    pub windows: Vec<&'w mut RenderedWindow>,
}

impl FloatingLayer<'_> {
    pub fn draw(
        &mut self,
        root_canvas: &Canvas,
        settings: &RendererSettings,
        default_background: Color,
        grid_scale: GridScale,
    ) -> Vec<WindowDrawDetails> {
        let pixel_regions = self
            .windows
            .iter()
            .map(|window| window.pixel_region(grid_scale))
            .collect::<Vec<_>>();
        let (silhouette, bound_rect) = build_silhouette(&pixel_regions, settings, grid_scale);
        let has_transparency = self.windows.iter().any(|window| window.has_transparency());

        self._draw_shadow(root_canvas, &silhouette, settings);

        root_canvas.save();
        root_canvas.clip_path(&silhouette, None, Some(false));
        let need_blur = has_transparency || settings.floating_blur;

        if need_blur {
            if let Some(blur) = blur(
                (
                    settings.floating_blur_amount_x,
                    settings.floating_blur_amount_y,
                ),
                None,
                None,
                None,
            ) {
                let paint = Paint::default()
                    .set_anti_alias(false)
                    .set_blend_mode(BlendMode::Src)
                    .to_owned();
                let save_layer_rec = SaveLayerRec::default()
                    .backdrop(&blur)
                    .bounds(&bound_rect)
                    .paint(&paint);
                root_canvas.save_layer(&save_layer_rec);
                root_canvas.restore();
            }
        }

        let paint = Paint::default()
            .set_anti_alias(false)
            .set_blend_mode(BlendMode::SrcOver)
            .to_owned();

        let save_layer_rec = SaveLayerRec::default().bounds(&bound_rect).paint(&paint);

        root_canvas.save_layer(&save_layer_rec);
        let background_paint = Paint::default().set_color(default_background).to_owned();
        root_canvas.draw_path(&silhouette, &background_paint);

        let regions = self
            .windows
            .iter()
            .map(|window| window.pixel_region(grid_scale))
            .collect::<Vec<_>>();

        let mut ret = vec![];

        (0..self.windows.len()).for_each(|i| {
            let window = &mut self.windows[i];
            window.draw_background_surface(root_canvas, regions[i], grid_scale);
            window.draw_foreground_surface(root_canvas, regions[i], grid_scale);
            ret.push(WindowDrawDetails {
                id: window.id,
                region: regions[i],
                grid_size: window.grid_size,
            });
        });

        root_canvas.restore();

        root_canvas.restore();

        ret
    }

    fn _draw_shadow(&self, root_canvas: &Canvas, path: &Path, settings: &RendererSettings) {
        if !settings.floating_shadow {
            return;
        }
        // Assume that the message window is the only one in the layer
        if self
            .windows
            .first()
            .is_some_and(|w| matches!(w.window_type, WindowType::Message { scrolled: false }))
        {
            return;
        }

        root_canvas.save();
        // We clip using the Difference op to make sure that the shadow isn't rendered inside
        // the window itself.
        root_canvas.clip_path(path, Some(ClipOp::Difference), None);
        // The light angle is specified in degrees from the vertical, so we first convert them
        // to radians and then use sin/cos to get the y and z components of the light
        let light_angle_radians = settings.light_angle_degrees.to_radians();
        draw_shadow(
            root_canvas,
            path,
            // Specifies how far from the root canvas the shadow casting rect is. We just use
            // the z component here to set it a constant distance away.
            Point3::new(0., 0., settings.floating_z_height),
            // Because we use the DIRECTIONAL_LIGHT shadow flag, this specifies the angle that
            // the light is coming from.
            Point3::new(0., -light_angle_radians.sin(), light_angle_radians.cos()),
            // This is roughly equal to the apparent radius of the light .
            5.,
            Color::from_argb((0.03 * 255.) as u8, 0, 0, 0),
            Color::from_argb((0.35 * 255.) as u8, 0, 0, 0),
            // Directional Light flag is necessary to make the shadow render consistently
            // across various sizes of floating windows. It effects how the light direction is
            // processed.
            Some(ShadowFlags::DIRECTIONAL_LIGHT),
        );
        root_canvas.restore();
    }
}

fn get_window_group(windows: &mut Vec<LayerWindow>, index: usize) -> usize {
    if windows[index].group != index {
        windows[index].group = get_window_group(windows, windows[index].group);
    }
    windows[index].group
}

fn group_windows_with_regions(windows: &mut Vec<LayerWindow>, regions: &[PixelRect<f32>]) {
    // intersects does not consider touching regions as intersection, so extend the box by one
    // pixel before doing the test.
    let epsilon = 1.0;
    for i in 0..windows.len() {
        for j in i + 1..windows.len() {
            let group_i = get_window_group(windows, i);
            let group_j = get_window_group(windows, j);
            if group_i != group_j
                && regions[i]
                    .to_rect()
                    .inflate((epsilon, epsilon).into())
                    .intersects(&regions[j])
            {
                let new_group = group_i.min(group_j);
                if group_i != group_j {
                    windows[group_i].group = new_group;
                    windows[group_j].group = new_group;
                }
            }
        }
    }
}

pub fn group_windows(
    windows: Vec<&mut RenderedWindow>,
    grid_scale: GridScale,
) -> Vec<Vec<&mut RenderedWindow>> {
    let mut windows = windows
        .into_iter()
        .enumerate()
        .map(|(index, window)| LayerWindow {
            window,
            group: index,
        })
        .collect::<Vec<_>>();
    let regions = windows
        .iter()
        .map(|window| window.window.pixel_region(grid_scale))
        .collect::<Vec<_>>();
    group_windows_with_regions(&mut windows, &regions);
    for i in 0..windows.len() {
        let _ = get_window_group(&mut windows, i);
    }
    windows.sort_by(|a, b| a.group.cmp(&b.group));
    windows
        .into_iter()
        .chunk_by(|window| window.group)
        .into_iter()
        .map(|(_, v)| v.map(|w| w.window).collect::<Vec<_>>())
        .collect_vec()
}

fn build_silhouette(
    regions: &[PixelRect<f32>],
    settings: &RendererSettings,
    grid_scale: GridScale,
) -> (Path, Rect) {
    let silhouette = regions
        .iter()
        .map(|r| rect_to_round_rect_path(to_skia_rect(r), settings, grid_scale))
        .reduce(|a, b| a.op(&b, PathOp::Union).unwrap())
        .unwrap();

    let bounding_rect = regions
        .iter()
        .map(to_skia_rect)
        .reduce(Rect::join2)
        .unwrap();

    (silhouette, bounding_rect)
}

fn rect_to_round_rect_path(rect: Rect, settings: &RendererSettings, grid_scale: GridScale) -> Path {
    let scaled_radius =
        if settings.floating_corner_radius > 0.0 && settings.floating_corner_radius <= 1.0 {
            settings.floating_corner_radius * grid_scale.height()
        } else {
            0.0
        };
    Path::rrect(RRect::new_rect_xy(rect, scaled_radius, scaled_radius), None)
}
