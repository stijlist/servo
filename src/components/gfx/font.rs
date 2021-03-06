/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

use azure::{AzFloat, AzScaledFontRef};
use azure::azure_hl::{BackendType, ColorPattern};
use azure::scaled_font::ScaledFont;
use geom::{Point2D, Rect, Size2D};
use std::mem;
use std::num::Zero;
use std::ptr;
use std::str;
use std::rc::Rc;
use std::cell::RefCell;
use servo_util::cache::{Cache, HashCache};
use servo_util::range::Range;
use style::computed_values::{text_decoration, font_weight, font_style};
use sync::Arc;

use color::Color;
use font_context::FontContext;
use servo_util::geometry::Au;
use platform::font_context::FontContextHandle;
use platform::font::{FontHandle, FontTable};
use render_context::RenderContext;
use text::glyph::{CharIndex, GlyphStore, GlyphId};
use text::shaping::ShaperMethods;
use text::{Shaper, TextRun};

#[cfg(target_os="linux")]
#[cfg(target_os="android")]
use azure::scaled_font::NativeFont;

// FontHandle encapsulates access to the platform's font API,
// e.g. quartz, FreeType. It provides access to metrics and tables
// needed by the text shaper as well as access to the underlying font
// resources needed by the graphics layer to draw glyphs.

pub trait FontHandleMethods {
    fn new_from_buffer(fctx: &FontContextHandle, buf: Vec<u8>, style: &SpecifiedFontStyle)
                    -> Result<Self,()>;

    // an identifier usable by FontContextHandle to recreate this FontHandle.
    fn face_identifier(&self) -> String;
    fn family_name(&self) -> String;
    fn face_name(&self) -> String;
    fn is_italic(&self) -> bool;
    fn boldness(&self) -> font_weight::T;

    fn glyph_index(&self, codepoint: char) -> Option<GlyphId>;
    fn glyph_h_advance(&self, GlyphId) -> Option<FractionalPixel>;
    fn get_metrics(&self) -> FontMetrics;
    fn get_table_for_tag(&self, FontTableTag) -> Option<FontTable>;
}

// Used to abstract over the shaper's choice of fixed int representation.
pub type FractionalPixel = f64;

pub type FontTableTag = u32;

pub trait FontTableTagConversions {
    fn tag_to_str(&self) -> String;
}

impl FontTableTagConversions for FontTableTag {
    fn tag_to_str(&self) -> String {
        unsafe {
            let reversed = str::raw::from_buf_len(mem::transmute(self), 4);
            return str::from_chars([reversed.as_slice().char_at(3),
                                    reversed.as_slice().char_at(2),
                                    reversed.as_slice().char_at(1),
                                    reversed.as_slice().char_at(0)]);
        }
    }
}

pub trait FontTableMethods {
    fn with_buffer(&self, |*u8, uint|);
}

#[deriving(Clone)]
pub struct FontMetrics {
    pub underline_size:   Au,
    pub underline_offset: Au,
    pub strikeout_size:   Au,
    pub strikeout_offset: Au,
    pub leading:          Au,
    pub x_height:         Au,
    pub em_size:          Au,
    pub ascent:           Au,
    pub descent:          Au,
    pub max_advance:      Au
}

// TODO(Issue #179): eventually this will be split into the specified
// and used font styles.  specified contains uninterpreted CSS font
// property values, while 'used' is attached to gfx::Font to descript
// the instance's properties.
//
// For now, the cases are differentiated with a typedef
#[deriving(Clone, PartialEq)]
pub struct FontStyle {
    pub pt_size: f64,
    pub weight: font_weight::T,
    pub style: font_style::T,
    pub families: Vec<String>,
    // TODO(Issue #198): font-stretch, text-decoration, font-variant, size-adjust
}

pub type SpecifiedFontStyle = FontStyle;
pub type UsedFontStyle = FontStyle;

// FontDescriptor serializes a specific font and used font style
// options, such as point size.

// It's used to swizzle/unswizzle gfx::Font instances when
// communicating across tasks, such as the display list between layout
// and render tasks.
#[deriving(Clone, PartialEq)]
pub struct FontDescriptor {
    pub style: UsedFontStyle,
    pub selector: FontSelector,
}

impl FontDescriptor {
    pub fn new(style: UsedFontStyle, selector: FontSelector) -> FontDescriptor {
        FontDescriptor {
            style: style,
            selector: selector,
        }
    }
}

// A FontSelector is a platform-specific strategy for serializing face names.
#[deriving(Clone, PartialEq)]
pub enum FontSelector {
    SelectorPlatformIdentifier(String),
}

// This struct is the result of mapping a specified FontStyle into the
// available fonts on the system. It contains an ordered list of font
// instances to be used in case the prior font cannot be used for
// rendering the specified language.

// The ordering of font instances is mainly decided by the CSS
// 'font-family' property. The last font is a system fallback font.
pub struct FontGroup {
    pub families: Vec<String>,
    // style of the first western font in group, which is
    // used for purposes of calculating text run metrics.
    pub style: UsedFontStyle,
    pub fonts: Vec<Rc<RefCell<Font>>>
}

impl FontGroup {
    pub fn new(families: Vec<String>, style: &UsedFontStyle, fonts: Vec<Rc<RefCell<Font>>>) -> FontGroup {
        FontGroup {
            families: families,
            style: (*style).clone(),
            fonts: fonts,
        }
    }

    pub fn create_textrun(&self, text: String, decoration: text_decoration::T) -> TextRun {
        assert!(self.fonts.len() > 0);

        // TODO(Issue #177): Actually fall back through the FontGroup when a font is unsuitable.
        TextRun::new(&mut *self.fonts.get(0).borrow_mut(), text.clone(), decoration)
    }
}

pub struct RunMetrics {
    // may be negative due to negative width (i.e., kerning of '.' in 'P.T.')
    pub advance_width: Au,
    pub ascent: Au, // nonzero
    pub descent: Au, // nonzero
    // this bounding box is relative to the left origin baseline.
    // so, bounding_box.position.y = -ascent
    pub bounding_box: Rect<Au>
}

impl RunMetrics {
    pub fn new(advance: Au, ascent: Au, descent: Au) -> RunMetrics {
        let bounds = Rect(Point2D(Au(0), -ascent),
                          Size2D(advance, ascent + descent));

        // TODO(Issue #125): support loose and tight bounding boxes; using the
        // ascent+descent and advance is sometimes too generous and
        // looking at actual glyph extents can yield a tighter box.

        RunMetrics {
            advance_width: advance,
            bounding_box: bounds,
            ascent: ascent,
            descent: descent,
        }
    }
}

/**
A font instance. Layout can use this to calculate glyph metrics
and the renderer can use it to render text.
*/
pub struct Font {
    pub handle: FontHandle,
    pub azure_font: Option<ScaledFont>,
    pub shaper: Option<Shaper>,
    pub style: UsedFontStyle,
    pub metrics: FontMetrics,
    pub backend: BackendType,
    pub shape_cache: HashCache<String, Arc<GlyphStore>>,
    pub glyph_advance_cache: HashCache<u32, FractionalPixel>,
}

impl<'a> Font {
    pub fn new_from_buffer(ctx: &FontContext,
                           buffer: Vec<u8>,
                           style: &SpecifiedFontStyle,
                           backend: BackendType)
            -> Result<Rc<RefCell<Font>>, ()> {
        let handle = FontHandleMethods::new_from_buffer(&ctx.handle, buffer, style);
        let handle: FontHandle = match handle {
            Ok(handle) => handle,
            Err(()) => return Err(()),
        };

        let metrics = handle.get_metrics();

        return Ok(Rc::new(RefCell::new(Font {
            handle: handle,
            azure_font: None,
            shaper: None,
            style: (*style).clone(),
            metrics: metrics,
            backend: backend,
            shape_cache: HashCache::new(),
            glyph_advance_cache: HashCache::new(),
        })));
    }

    pub fn new_from_adopted_handle(_fctx: &FontContext, handle: FontHandle,
                               style: &SpecifiedFontStyle, backend: BackendType)
                               -> Font {
        let metrics = handle.get_metrics();

        Font {
            handle: handle,
            azure_font: None,
            shaper: None,
            style: (*style).clone(),
            metrics: metrics,
            backend: backend,
            shape_cache: HashCache::new(),
            glyph_advance_cache: HashCache::new(),
        }
    }

    fn make_shaper(&'a mut self) -> &'a Shaper {
        // fast path: already created a shaper
        match self.shaper {
            Some(ref shaper) => {
                let s: &'a Shaper = shaper;
                return s;
            },
            None => {}
        }

        let shaper = Shaper::new(self);
        self.shaper = Some(shaper);
        self.shaper.get_ref()
    }

    pub fn get_table_for_tag(&self, tag: FontTableTag) -> Option<FontTable> {
        let result = self.handle.get_table_for_tag(tag);
        let status = if result.is_some() { "Found" } else { "Didn't find" };

        debug!("{:s} font table[{:s}] with family={}, face={}",
               status, tag.tag_to_str(),
               self.handle.family_name(), self.handle.face_name());

        return result;
    }

    // TODO: this should return a borrowed pointer, but I can't figure
    // out why borrowck doesn't like my implementation.

    fn get_azure_font(&mut self) -> AzScaledFontRef {
        // fast path: we've already created the azure font resource
        match self.azure_font {
            Some(ref azfont) => return azfont.get_ref(),
            None => {}
        }

        let scaled_font = self.create_azure_font();
        self.azure_font = Some(scaled_font);
        // try again.
        return self.get_azure_font();
    }

    #[cfg(target_os="macos")]
    fn create_azure_font(&mut self) -> ScaledFont {
        let cg_font = self.handle.get_CGFont();
        let size = self.style.pt_size as AzFloat;
        ScaledFont::new(self.backend, &cg_font, size)
    }

    #[cfg(target_os="linux")]
    #[cfg(target_os="android")]
    fn create_azure_font(&self) -> ScaledFont {
        let freetype_font = self.handle.face;
        let size = self.style.pt_size as AzFloat;
        ScaledFont::new(self.backend, NativeFont(freetype_font), size)
    }
}


impl Font {
    pub fn draw_text_into_context(&mut self,
                              rctx: &RenderContext,
                              run: &Box<TextRun>,
                              range: &Range<CharIndex>,
                              baseline_origin: Point2D<Au>,
                              color: Color) {
        use libc::types::common::c99::{uint16_t, uint32_t};
        use azure::{struct__AzDrawOptions,
                    struct__AzGlyph,
                    struct__AzGlyphBuffer,
                    struct__AzPoint};
        use azure::azure::{AzDrawTargetFillGlyphs};

        let target = rctx.get_draw_target();
        let azfontref = self.get_azure_font();
        let pattern = ColorPattern::new(color);
        let azure_pattern = pattern.azure_color_pattern;
        assert!(azure_pattern.is_not_null());

        let options = struct__AzDrawOptions {
            mAlpha: 1f64 as AzFloat,
            fields: 0x0200 as uint16_t
        };

        let mut origin = baseline_origin.clone();
        let mut azglyphs = vec!();
        azglyphs.reserve(range.length().to_uint());

        for (glyphs, _offset, slice_range) in run.iter_slices_for_range(range) {
            for (_i, glyph) in glyphs.iter_glyphs_for_char_range(&slice_range) {
                let glyph_advance = glyph.advance();
                let glyph_offset = glyph.offset().unwrap_or(Zero::zero());

                let azglyph = struct__AzGlyph {
                    mIndex: glyph.id() as uint32_t,
                    mPosition: struct__AzPoint {
                        x: (origin.x + glyph_offset.x).to_nearest_px() as AzFloat,
                        y: (origin.y + glyph_offset.y).to_nearest_px() as AzFloat
                    }
                };
                origin = Point2D(origin.x + glyph_advance, origin.y);
                azglyphs.push(azglyph)
            };
        }

        let azglyph_buf_len = azglyphs.len();
        if azglyph_buf_len == 0 { return; } // Otherwise the Quartz backend will assert.

        let glyphbuf = struct__AzGlyphBuffer {
            mGlyphs: azglyphs.as_ptr(),
            mNumGlyphs: azglyph_buf_len as uint32_t
        };

        unsafe {
            // TODO(Issue #64): this call needs to move into azure_hl.rs
            AzDrawTargetFillGlyphs(target.azure_draw_target,
                                   azfontref,
                                   &glyphbuf,
                                   azure_pattern,
                                   &options,
                                   ptr::null());
        }
    }

    pub fn measure_text(&self, run: &TextRun, range: &Range<CharIndex>) -> RunMetrics {
        // TODO(Issue #199): alter advance direction for RTL
        // TODO(Issue #98): using inter-char and inter-word spacing settings  when measuring text
        let mut advance = Au(0);
        for (glyphs, _offset, slice_range) in run.iter_slices_for_range(range) {
            for (_i, glyph) in glyphs.iter_glyphs_for_char_range(&slice_range) {
                advance = advance + glyph.advance();
            }
        }
        RunMetrics::new(advance, self.metrics.ascent, self.metrics.descent)
    }

    pub fn measure_text_for_slice(&self,
                                  glyphs: &GlyphStore,
                                  slice_range: &Range<CharIndex>)
                                  -> RunMetrics {
        let mut advance = Au(0);
        for (_i, glyph) in glyphs.iter_glyphs_for_char_range(slice_range) {
            advance = advance + glyph.advance();
        }
        RunMetrics::new(advance, self.metrics.ascent, self.metrics.descent)
    }

    pub fn shape_text(&mut self, text: String, is_whitespace: bool) -> Arc<GlyphStore> {

        //FIXME (ksh8281)
        self.make_shaper();
        let shaper = &self.shaper;
        self.shape_cache.find_or_create(&text, |txt| {
            let mut glyphs = GlyphStore::new(text.as_slice().char_len() as int, is_whitespace);
            shaper.get_ref().shape_text(txt.as_slice(), &mut glyphs);
            Arc::new(glyphs)
        })
    }

    pub fn get_descriptor(&self) -> FontDescriptor {
        FontDescriptor::new(self.style.clone(), SelectorPlatformIdentifier(self.handle.face_identifier()))
    }

    pub fn glyph_index(&self, codepoint: char) -> Option<GlyphId> {
        self.handle.glyph_index(codepoint)
    }

    pub fn glyph_h_advance(&mut self, glyph: GlyphId) -> FractionalPixel {
        let handle = &self.handle;
        self.glyph_advance_cache.find_or_create(&glyph, |glyph| {
            match handle.glyph_h_advance(*glyph) {
                Some(adv) => adv,
                None => /* FIXME: Need fallback strategy */ 10f64 as FractionalPixel
            }
        })
    }
}

