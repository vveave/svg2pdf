#![deny(missing_docs)]

/*! Convert SVG files to PDFs.

This crate allows to convert static (i.e. non-interactive) SVG files to
either standalone PDF files or Form XObjects that can be embedded in another
PDF file and used just like images.

The conversion will translate the SVG content to PDF without rasterizing them
(the only exception being objects with filters on them, but in this case only
this single group will be rasterized, while the remaining contents of the SVG
will still be turned into a vector graphic), so no quality is lost.

## Example
This example reads an SVG file and writes the corresponding PDF back to the disk.

```
# fn main() -> Result<(), Box<dyn std::error::Error>> {
use svg2pdf::{ConversionOptions, PageOptions};

let input = "tests/svg/custom/integration/matplotlib/stairs.svg";
let output = "target/stairs.pdf";

let svg = std::fs::read_to_string(input)?;
let mut options = svg2pdf::usvg::Options::default();
options.fontdb_mut().load_system_fonts();
let tree = svg2pdf::usvg::Tree::from_str(&svg, &options)?;

let pdf = svg2pdf::to_pdf(&tree, ConversionOptions::default(), PageOptions::default())?;
std::fs::write(output, pdf)?;
# Ok(()) }
```

## Supported features
In general, a very large part of the SVG specification is supported, including
but not limited to:
- Paths with simple and complex fills
- Gradients
- Patterns
- Clip paths
- Masks
- Transformations
- Viewbox
- Text
- Raster images and nested SVGs

## Unsupported features
Among the unsupported features are currently:
- The `spreadMethod` attribute of gradients
- Raster images are not color managed but use PDF's DeviceRGB color space
- A number of features that were added in SVG2, See
  [here](https://github.com/RazrFalcon/resvg/blob/master/docs/svg2-changelog.md) for a more
  comprehensive list.
 */

mod render;
mod util;

use std::error::Error;
use std::fmt;
use std::fmt::{Display, Formatter};
/// The SVG parser and tree representation used by this crate.
///
/// Re-exporting `usvg` ensures callers can construct a compatible [`usvg::Tree`]
/// without depending on a potentially incompatible version themselves.
pub use usvg;

use crate::ConversionError::UnknownError;
use once_cell::sync::Lazy;
use pdf_writer::{Chunk, Content, Filter, Finish, Pdf, Ref, TextStr};
use usvg::{Size, Transform, Tree};

use crate::render::{tree_to_stream, tree_to_xobject};
use crate::util::context::Context;
use crate::util::helper::{deflate, ContentExt, RectExt, TransformExt};
use crate::util::resources::ResourceContainer;

// The ICC profiles.
static SRGB_ICC_DEFLATED: Lazy<Vec<u8>> =
    Lazy::new(|| deflate(include_bytes!("icc/sRGB-v4.icc")));
static GRAY_ICC_DEFLATED: Lazy<Vec<u8>> =
    Lazy::new(|| deflate(include_bytes!("icc/sGrey-v4.icc")));

/// Page-level options used by [`to_pdf`].
///
/// These options do not apply to [`to_chunk`], which creates a reusable Form
/// XObject rather than a standalone page.
#[derive(Copy, Clone)]
pub struct PageOptions {
    /// The DPI that should be assumed for the conversion to PDF.
    ///
    /// Must be finite and greater than zero.
    ///
    /// _Default:_ `72.0`.
    pub dpi: f32,
}

impl Default for PageOptions {
    fn default() -> Self {
        Self { dpi: 72.0 }
    }
}

impl PageOptions {
    fn validate(self) -> Result<()> {
        if !self.dpi.is_finite() || self.dpi <= 0.0 {
            return Err(ConversionError::InvalidDpi);
        }
        Ok(())
    }
}

/// An error that can occur during conversion.
#[derive(Copy, Clone, Debug)]
pub enum ConversionError {
    /// The SVG contains an invalid or unsupported embedded image.
    InvalidImage,
    /// Text shaping produced a `.notdef` glyph while PDF/A processing was enabled.
    MissingGlyphs,
    /// Converting the SVG would exceed PDF's supported graphics-state nesting depth.
    TooMuchNesting,
    /// The configured page DPI is not finite and greater than zero.
    InvalidDpi,
    /// The configured filter raster scale is not finite and greater than zero.
    InvalidRasterScale,
    /// A filter would require a temporary raster image larger than 16,777,216 pixels.
    FilterRegionTooLarge,
    /// An internal conversion invariant was violated.
    ///
    /// Receiving this error can indicate a bug in `svg2pdf`.
    UnknownError,
    /// The identified font could not be subset for embedding.
    #[cfg(feature = "text")]
    SubsetError(fontdb::ID),
    /// The identified font could not be parsed.
    #[cfg(feature = "text")]
    InvalidFont(fontdb::ID),
}

impl Display for ConversionError {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        match self {
            Self::InvalidImage => f.write_str("An unknown type of image appears in the SVG."),
            Self::MissingGlyphs => f.write_str("A piece of text could not be displayed with any font."),
            Self::TooMuchNesting => f.write_str("The SVG's nesting depth is too high."),
            Self::InvalidDpi => f.write_str("The page DPI must be finite and greater than zero."),
            Self::InvalidRasterScale => {
                f.write_str("The filter raster scale must be finite and greater than zero.")
            }
            Self::FilterRegionTooLarge => {
                f.write_str("A filter region exceeds the rasterization pixel limit.")
            }
            Self::UnknownError => f.write_str("An unknown error occurred during the conversion. This could indicate a bug in svg2pdf"),
            #[cfg(feature = "text")]
            Self::SubsetError(_) => f.write_str("An error occurred while subsetting a font."),
            #[cfg(feature = "text")]
            Self::InvalidFont(_) => f.write_str("An error occurred while reading a font."),
        }
    }
}

impl Error for ConversionError {}

/// The result type for everything.
type Result<T> = std::result::Result<T, ConversionError>;

/// Options for the PDF conversion.
#[derive(Copy, Clone)]
pub struct ConversionOptions {
    /// Whether the content streams should be compressed.
    ///
    /// The smaller PDFs generated by this are generally more practical, but it
    /// might increase run-time a bit.
    ///
    /// _Default:_ `true`.
    pub compress: bool,

    /// How much raster images of rasterized effects should be scaled up.
    ///
    /// Higher values will lead to better quality, but will increase the size of
    /// the pdf. Must be finite and greater than zero. Filter rasterization is
    /// rejected if the resulting temporary image exceeds the pixel limit.
    ///
    /// This option has no effect when the `filters` feature is disabled.
    ///
    /// _Default:_ `1.5`.
    pub raster_scale: f32,

    /// Whether text should be embedded as actual selectable text inside
    /// the PDF. If this option is disabled, text will be converted into paths
    /// before rendering.
    ///
    /// This option has no effect when the `text` feature is disabled.
    ///
    /// _Default:_ `true`.
    pub embed_text: bool,

    /// Whether to write chunks in PDF/A-2b compliant mode.
    ///
    /// **Note:** This currently only ensures that `to_chunk` does not generate
    /// anything that is forbidden by PDF/A. It does _not_ turn the
    /// free-standing PDF generated by [`to_pdf`] into a valid PDF/A document.
    ///
    /// _Default:_ `false`.
    pub pdfa: bool,
}

impl ConversionOptions {
    fn validate(self) -> Result<()> {
        if !self.raster_scale.is_finite() || self.raster_scale <= 0.0 {
            return Err(ConversionError::InvalidRasterScale);
        }
        Ok(())
    }
}

impl Default for ConversionOptions {
    fn default() -> Self {
        Self {
            compress: true,
            raster_scale: 1.5,
            embed_text: true,
            pdfa: false,
        }
    }
}

/// Convert a [`usvg` tree](Tree) into a standalone PDF buffer.
///
/// # Errors
///
/// Returns:
///
/// - [`ConversionError::InvalidDpi`] when `page_options.dpi` is not finite and
///   greater than zero.
/// - [`ConversionError::InvalidRasterScale`] when
///   `conversion_options.raster_scale` is not finite and greater than zero.
/// - [`ConversionError::FilterRegionTooLarge`] when rasterizing an SVG filter
///   would exceed the temporary image pixel limit.
/// - Another [`ConversionError`] when images, fonts, geometry, or PDF state
///   cannot be converted safely.
///
/// ## Example
/// The example below reads an SVG file, processes text within it, then converts
/// it into a PDF and finally writes it back to the file system.
///
/// ```
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// use svg2pdf::{ConversionOptions, PageOptions};
///
/// let input = "tests/svg/custom/integration/matplotlib/stairs.svg";
/// let output = "target/stairs.pdf";
///
/// let svg = std::fs::read_to_string(input)?;
/// let mut options = svg2pdf::usvg::Options::default();
/// options.fontdb_mut().load_system_fonts();
/// let tree = svg2pdf::usvg::Tree::from_str(&svg, &options)?;
///
/// let pdf = svg2pdf::to_pdf(&tree, ConversionOptions::default(), PageOptions::default())?;
/// std::fs::write(output, pdf)?;
/// # Ok(()) }
/// ```
pub fn to_pdf(
    tree: &Tree,
    conversion_options: ConversionOptions,
    page_options: PageOptions,
) -> Result<Vec<u8>> {
    conversion_options.validate()?;
    page_options.validate()?;
    let mut ctx = Context::new(tree, conversion_options)?;
    let mut pdf = Pdf::new();

    let dpi_ratio = 72.0 / page_options.dpi;
    let dpi_transform = Transform::from_scale(dpi_ratio, dpi_ratio);
    let page_size =
        Size::from_wh(tree.size().width() * dpi_ratio, tree.size().height() * dpi_ratio)
            .ok_or(UnknownError)?;

    let catalog_ref = ctx.alloc_ref();
    let page_tree_ref = ctx.alloc_ref();
    let page_ref = ctx.alloc_ref();
    let content_ref = ctx.alloc_ref();

    pdf.catalog(catalog_ref).pages(page_tree_ref);
    pdf.pages(page_tree_ref).count(1).kids([page_ref]);

    // Generate main content
    let mut rc = ResourceContainer::new();
    let mut content = Content::new();
    content.save_state_checked()?;
    content.transform(dpi_transform.to_pdf_transform());
    tree_to_stream(tree, &mut pdf, &mut content, &mut ctx, &mut rc)?;
    content.restore_state();
    let content_stream = ctx.finish_content(content);
    let mut stream = pdf.stream(content_ref, &content_stream);

    if ctx.options.compress {
        stream.filter(Filter::FlateDecode);
    }
    stream.finish();

    let mut page = pdf.page(page_ref);
    let mut page_resources = page.resources();
    rc.finish(&mut page_resources);
    page_resources.finish();

    page.media_box(page_size.to_non_zero_rect(0.0, 0.0).to_pdf_rect());
    page.parent(page_tree_ref);
    page.group()
        .transparency()
        .isolated(true)
        .knockout(false)
        .color_space()
        .icc_based(ctx.srgb_ref());
    page.contents(content_ref);
    page.finish();

    ctx.write_global_objects(&mut pdf)?;

    let document_info_id = ctx.alloc_ref();
    pdf.document_info(document_info_id).producer(TextStr("svg2pdf"));

    Ok(pdf.finish())
}

/// Convert a [Tree] into a [`Chunk`].
///
/// This method is intended for use in an existing [`pdf_writer`] workflow. It
/// will always produce a chunk that contains all the necessary objects
/// to embed the SVG into an existing chunk. This method returns the chunk that
/// was produced as part of that as well as the object reference of the root XObject.
/// The XObject will have the width and height of one printer's
/// point, just like an [`ImageXObject`](pdf_writer::writers::ImageXObject)
/// would.
///
/// The resulting object can be used by embedding the chunk into your existing chunk
/// and renumbering it appropriately.
///
/// # Errors
///
/// Returns:
///
/// - [`ConversionError::InvalidRasterScale`] when
///   `conversion_options.raster_scale` is not finite and greater than zero.
/// - [`ConversionError::FilterRegionTooLarge`] when rasterizing an SVG filter
///   would exceed the temporary image pixel limit.
/// - Another [`ConversionError`] when images, fonts, geometry, or PDF state
///   cannot be converted safely.
///
/// ## Example
/// Write a PDF file with some text and an SVG graphic.
///
/// ```
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// use std::collections::HashMap;
/// use svg2pdf;
/// use pdf_writer::{Content, Finish, Name, Pdf, Rect, Ref, Str};
///
/// // Allocate the indirect reference IDs and names.
/// let mut alloc = Ref::new(1);
/// let catalog_id = alloc.bump();
/// let page_tree_id = alloc.bump();
/// let page_id = alloc.bump();
/// let font_id = alloc.bump();
/// let content_id = alloc.bump();
/// let font_name = Name(b"F1");
/// let svg_name = Name(b"S1");
///
/// // Let's first convert the SVG into an independent chunk.
/// let path = "tests/svg/custom/integration/wikimedia/coat_of_the_arms_of_edinburgh_city_council.svg";
/// let svg = std::fs::read_to_string(path)?;
/// let mut options = svg2pdf::usvg::Options::default();
/// options.fontdb_mut().load_system_fonts();
/// let tree = svg2pdf::usvg::Tree::from_str(&svg, &options)?;
/// let (mut svg_chunk, svg_id) = svg2pdf::to_chunk(&tree, svg2pdf::ConversionOptions::default())?;
///
/// // Renumber the chunk so that we can embed it into our existing workflow, and also make sure
/// // to update `svg_id`.
/// let mut map = HashMap::new();
/// let svg_chunk = svg_chunk.renumber(|old| {
///   *map.entry(old).or_insert_with(|| alloc.bump())
/// });
/// let svg_id = *map
///     .get(&svg_id)
///     .ok_or_else(|| std::io::Error::other("renumbered SVG reference is missing"))?;
///
/// // Start writing the PDF.
/// let mut pdf = Pdf::new();
/// pdf.catalog(catalog_id).pages(page_tree_id);
/// pdf.pages(page_tree_id).kids([page_id]).count(1);
///
/// // Set up a simple A4 page.
/// let mut page = pdf.page(page_id);
/// page.media_box(Rect::new(0.0, 0.0, 595.0, 842.0));
/// page.parent(page_tree_id);
/// page.contents(content_id);
///
/// // Add the font and, more importantly, the SVG to the resource dictionary
/// // so that it can be referenced in the content stream.
/// let mut resources = page.resources();
/// resources.x_objects().pair(svg_name, svg_id);
/// resources.fonts().pair(font_name, font_id);
/// resources.finish();
/// page.finish();
///
/// // Set a predefined font, so we do not have to load anything extra.
/// pdf.type1_font(font_id).base_font(Name(b"Helvetica"));
///
/// // Write a content stream with some text and our SVG.
/// let mut content = Content::new();
/// content
///     .begin_text()
///     .set_font(font_name, 16.0)
///     .next_line(108.0, 734.0)
///     .show(Str(b"Look at my wonderful (distorted) vector graphic!"))
///     .end_text();
///
/// // Add our graphic.
/// content
///     .transform([300.0, 0.0, 0.0, 225.0, 147.5, 385.0])
///     .x_object(svg_name);
///
/// pdf.stream(content_id, &content.finish());
/// // Write the SVG chunk into the PDF page.
/// pdf.extend(&svg_chunk);
///
/// // Write the file to the disk.
/// std::fs::write("target/embedded.pdf", pdf.finish())?;
/// # Ok(()) }
/// ```
pub fn to_chunk(
    tree: &Tree,
    conversion_options: ConversionOptions,
) -> Result<(Chunk, Ref)> {
    conversion_options.validate()?;
    let mut chunk = Chunk::new();

    let mut ctx = Context::new(tree, conversion_options)?;
    let x_ref = tree_to_xobject(tree, &mut chunk, &mut ctx)?;
    ctx.write_global_objects(&mut chunk)?;
    Ok((chunk, x_ref))
}

#[cfg(test)]
mod option_tests {
    use super::*;

    fn test_tree() -> std::result::Result<Tree, usvg::Error> {
        Tree::from_str(
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="1" height="1"/>"#,
            &usvg::Options::default(),
        )
    }

    #[test]
    fn to_pdf_rejects_invalid_dpi() -> std::result::Result<(), Box<dyn Error>> {
        let tree = test_tree()?;

        for dpi in [0.0, -1.0, f32::NAN, f32::INFINITY] {
            let result = to_pdf(&tree, ConversionOptions::default(), PageOptions { dpi });
            assert!(matches!(result, Err(ConversionError::InvalidDpi)));
        }
        Ok(())
    }

    #[test]
    fn conversion_rejects_invalid_raster_scale() -> std::result::Result<(), Box<dyn Error>>
    {
        let tree = test_tree()?;

        for raster_scale in [0.0, -1.0, f32::NAN, f32::INFINITY] {
            let options =
                ConversionOptions { raster_scale, ..ConversionOptions::default() };
            let result = to_chunk(&tree, options);
            assert!(matches!(result, Err(ConversionError::InvalidRasterScale)));
        }
        Ok(())
    }
}
