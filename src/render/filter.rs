use crate::render::image;
use crate::util::context::Context;
use crate::util::resources::ResourceContainer;
use crate::ConversionError::{FilterRegionTooLarge, UnknownError};
use crate::Result;
use pdf_writer::{Chunk, Content};
use std::sync::Arc;
use tiny_skia::{Size, Transform};
use usvg::{Group, ImageKind, Node};

/// Keep temporary filter pixmaps below 64 MiB (four bytes per pixel).
const MAX_FILTER_PIXELS: u64 = 16 * 1024 * 1024;

fn filter_pixmap_dimensions(size: Size) -> Result<(u32, u32)> {
    let width = size.width().round().max(1.0) as u32;
    let height = size.height().round().max(1.0) as u32;
    let pixels = u64::from(width) * u64::from(height);

    if pixels > MAX_FILTER_PIXELS {
        return Err(FilterRegionTooLarge);
    }

    Ok((width, height))
}

/// Render a group with filters as an image.
pub fn render(
    group: &Group,
    chunk: &mut Chunk,
    content: &mut Content,
    ctx: &mut Context,
    rc: &mut ResourceContainer,
) -> Result<()> {
    let layer_bbox = group
        .layer_bounding_box()
        .transform(group.transform())
        .ok_or(UnknownError)?;
    let pixmap_size = Size::from_wh(
        layer_bbox.width() * ctx.options.raster_scale,
        layer_bbox.height() * ctx.options.raster_scale,
    )
    .ok_or(UnknownError)?;

    let (pixmap_width, pixmap_height) = filter_pixmap_dimensions(pixmap_size)?;
    let mut pixmap = tiny_skia::Pixmap::new(pixmap_width, pixmap_height)
        .ok_or(FilterRegionTooLarge)?;

    let initial_transform =
        Transform::from_scale(ctx.options.raster_scale, ctx.options.raster_scale)
            .pre_concat(Transform::from_translate(-layer_bbox.x(), -layer_bbox.y()))
            // This one is a hack because resvg::render_node will take the absolute layer bbox into consideration
            // and translate by -layer_bbox.x() and -layer_bbox.y(), but we don't want that, so we
            // inverse it.
            .pre_concat(Transform::from_translate(
                group.abs_layer_bounding_box().x(),
                group.abs_layer_bounding_box().y(),
            ));

    resvg::render_node(
        &Node::Group(Box::new(group.clone())),
        initial_transform,
        &mut pixmap.as_mut(),
    );

    let encoded_image = pixmap.encode_png().map_err(|_| UnknownError)?;

    image::render(
        true,
        &ImageKind::PNG(Arc::new(encoded_image)),
        Some(layer_bbox.to_rect()),
        chunk,
        content,
        ctx,
        rc,
    )?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_filter_pixmap_above_pixel_budget() {
        let size = Size::from_wh(4097.0, 4097.0);
        assert!(matches!(
            size.map(filter_pixmap_dimensions),
            Some(Err(FilterRegionTooLarge))
        ));
    }

    #[test]
    fn accepts_filter_pixmap_at_pixel_budget() {
        let size = Size::from_wh(4096.0, 4096.0);
        assert_eq!(
            size.and_then(|size| filter_pixmap_dimensions(size).ok()),
            Some((4096, 4096))
        );
    }
}
