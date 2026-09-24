// Text stays in memory for one page; no OCR, rasterization, or persistent index.
use hayro::hayro_interpret::{
    font::Glyph, hayro_cmap::BfString, interpret_page, BlendMode, ClipPath, Context,
    Device, GlyphDrawMode, Image, InterpreterCache, InterpreterSettings, Paint, PathDrawMode, SoftMask,
    RectExt,
};
use hayro::hayro_syntax::page::Page;
use hayro::vello_cpu::kurbo::{Affine, BezPath, Point};

#[derive(Default)]
struct TextDevice {
    text: String,
    previous: Option<Point>,
    truncated: bool,
}

impl Device<'_> for TextDevice {
    fn set_soft_mask(&mut self, _: Option<SoftMask<'_>>) {}
    fn set_blend_mode(&mut self, _: BlendMode) {}
    fn draw_path(&mut self, _: &BezPath, _: Affine, _: &Paint<'_>, _: &PathDrawMode) {}
    fn push_clip_path(&mut self, _: &ClipPath) {}
    fn push_transparency_group(&mut self, _: f32, _: Option<SoftMask<'_>>, _: BlendMode) {}
    fn draw_image(&mut self, _: Image<'_, '_>, _: Affine) {}
    fn pop_clip_path(&mut self) {}
    fn pop_transparency_group(&mut self) {}
    fn draw_glyph(&mut self, glyph: &Glyph<'_>, transform: Affine, _: Affine, _: &Paint<'_>, _: &GlyphDrawMode) {
        if self.text.len() >= 800_000 { self.truncated = true; return; }
        let Some(value) = glyph.as_unicode() else { return; };
        let position = transform * Point::ZERO;
        if self.previous.is_some_and(|previous| (position.y - previous.y).abs() > 2.0) {
            self.text.push('\n');
        }
        self.previous = Some(position);
        match value {
            BfString::Char(character) => {
                if !character.is_control() || character.is_whitespace() { self.text.push(character); }
            }
            BfString::String(value) => self.text.extend(value.chars().filter(|c| !c.is_control() || c.is_whitespace())),
        }
    }
}

pub(super) fn extract<'a>(page: &Page<'a>, cache: &InterpreterCache<'a>) -> Result<String, String> {
    // A malformed page must not discard indexed results or terminate the native worker.
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut context = Context::new(Affine::IDENTITY, page.media_box().to_kurbo(), cache, page.xref(), InterpreterSettings::default());
        let mut device = TextDevice::default();
        interpret_page(page, &mut context, &mut device);
        if device.truncated || device.text.len() > 800_000 { Err("teaching_source_text_limit".into()) } else { Ok(device.text) }
    })).unwrap_or_else(|_| Err("teaching_source_text_extraction_failed".into()))
}
