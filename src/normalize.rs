use crate::{error::IconError, model::CANVAS_SIZE};
use image::{DynamicImage, ImageEncoder, Rgba, RgbaImage, imageops};
use std::io::Cursor;

pub fn normalize_to_png(bytes: &[u8], mime_type: &str) -> Result<Vec<u8>, IconError> {
    let image = if mime_type == "image/svg+xml" {
        rasterize_svg(bytes)?
    } else {
        image::load_from_memory(bytes)
            .map_err(|error| IconError::InvalidImage(error.to_string()))?
    };

    let image = fit_canvas(image);
    encode_png(&image)
}

pub fn fit_canvas(image: DynamicImage) -> RgbaImage {
    let image = image.to_rgba8();
    let scale =
        (CANVAS_SIZE as f32 / image.width() as f32).min(CANVAS_SIZE as f32 / image.height() as f32);
    let width = ((image.width() as f32 * scale).round() as u32).max(1);
    let height = ((image.height() as f32 * scale).round() as u32).max(1);
    let resized = imageops::resize(&image, width, height, imageops::FilterType::Lanczos3);
    let mut canvas = RgbaImage::from_pixel(CANVAS_SIZE, CANVAS_SIZE, Rgba([0, 0, 0, 0]));
    let x = (CANVAS_SIZE - width) / 2;
    let y = (CANVAS_SIZE - height) / 2;
    imageops::overlay(&mut canvas, &resized, i64::from(x), i64::from(y));
    canvas
}

pub fn encode_png(image: &RgbaImage) -> Result<Vec<u8>, IconError> {
    let mut output = Cursor::new(Vec::new());
    image::codecs::png::PngEncoder::new(&mut output)
        .write_image(
            image.as_raw(),
            image.width(),
            image.height(),
            image::ColorType::Rgba8.into(),
        )
        .map_err(|error| IconError::InvalidImage(error.to_string()))?;
    Ok(output.into_inner())
}

pub fn has_transparency(bytes: &[u8]) -> Result<bool, IconError> {
    let image = image::load_from_memory(bytes)
        .map_err(|error| IconError::InvalidImage(error.to_string()))?
        .to_rgba8();
    Ok(image.pixels().any(|pixel| pixel[3] < 255))
}

fn rasterize_svg(bytes: &[u8]) -> Result<DynamicImage, IconError> {
    let options = resvg::usvg::Options::default();
    let tree = resvg::usvg::Tree::from_data(bytes, &options)
        .map_err(|error| IconError::InvalidSvg(error.to_string()))?;
    let size = tree.size().to_int_size();
    let mut pixmap = resvg::tiny_skia::Pixmap::new(size.width(), size.height())
        .ok_or_else(|| IconError::InvalidSvg("could not allocate SVG canvas".to_owned()))?;
    resvg::render(
        &tree,
        resvg::tiny_skia::Transform::identity(),
        &mut pixmap.as_mut(),
    );
    let image = RgbaImage::from_raw(size.width(), size.height(), pixmap.take())
        .ok_or_else(|| IconError::InvalidSvg("invalid rasterized SVG".to_owned()))?;
    Ok(DynamicImage::ImageRgba8(image))
}
