use crate::{
    error::IconError,
    model::{CANVAS_SIZE, LayerArtifact},
};
use image::RgbaImage;
use roxmltree::Document;

#[derive(Debug, Clone)]
pub struct RasterLayer {
    pub id: String,
    pub image: RgbaImage,
}

pub fn validate_svg(svg: &str) -> Result<Vec<LayerArtifact>, IconError> {
    let layers = validate_svg_structure(svg)?;
    let tree = parse_tree(svg)?;
    let background = render_node_to_canvas(&tree, "background")?;
    if background.pixels().any(|pixel| pixel[3] != 255) {
        return Err(invalid(
            "background layer must cover the full canvas and be opaque",
        ));
    }
    Ok(layers)
}

pub fn validate_svg_structure(svg: &str) -> Result<Vec<LayerArtifact>, IconError> {
    let document =
        Document::parse(svg).map_err(|error| IconError::InvalidSvg(error.to_string()))?;
    let root = document.root_element();
    if root.tag_name().name() != "svg" {
        return Err(invalid("root element must be svg"));
    }
    let view_box = root
        .attribute("viewBox")
        .ok_or_else(|| invalid("missing viewBox"))?;
    let values = view_box
        .split(|character: char| character.is_ascii_whitespace() || character == ',')
        .filter(|value| !value.is_empty())
        .map(str::parse::<f32>)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| invalid("invalid viewBox"))?;
    if values.as_slice() != [0.0, 0.0, CANVAS_SIZE as f32, CANVAS_SIZE as f32] {
        return Err(invalid("viewBox must be 0 0 1024 1024"));
    }

    for node in root.descendants().filter(|node| node.is_element()) {
        match node.tag_name().name() {
            "script" | "foreignObject" | "image" | "text" | "style" | "filter" | "mask" => {
                return Err(invalid(format!(
                    "{} elements are not allowed",
                    node.tag_name().name()
                )));
            }
            _ => {}
        }
        for attribute in node.attributes() {
            let value = attribute.value().trim();
            if matches!(attribute.name(), "href" | "xlink:href") && !value.starts_with('#') {
                return Err(invalid("external references are not allowed"));
            }
            if value.contains("url(") && !value.contains("url(#") {
                return Err(invalid("external URLs are not allowed"));
            }
        }
    }

    let groups = root
        .children()
        .filter(|node| node.is_element() && node.tag_name().name() != "defs")
        .collect::<Vec<_>>();
    if !(2..=5).contains(&groups.len()) || groups.iter().any(|node| node.tag_name().name() != "g") {
        return Err(invalid(
            "SVG must contain a background plus one to four foreground groups",
        ));
    }
    let mut layers = Vec::with_capacity(groups.len());
    for (index, group) in groups.into_iter().enumerate() {
        let expected = if index == 0 {
            "background".to_owned()
        } else {
            format!("foreground-{index}")
        };
        if group.attribute("id") != Some(expected.as_str()) {
            return Err(invalid(format!("layer {index} must be named {expected}")));
        }
        layers.push(LayerArtifact {
            id: expected,
            z_index: index as u8,
        });
    }

    let tree = parse_tree(svg)?;
    if !tree.filters().is_empty() {
        return Err(invalid("filters are not allowed"));
    }
    Ok(layers)
}

pub fn rasterize_layers(svg: &str) -> Result<Vec<RasterLayer>, IconError> {
    let layers = validate_svg(svg)?;
    let tree = parse_tree(svg)?;
    layers
        .into_iter()
        .map(|layer| {
            Ok(RasterLayer {
                image: render_node_to_canvas(&tree, &layer.id)?,
                id: layer.id,
            })
        })
        .collect()
}

fn parse_tree(svg: &str) -> Result<resvg::usvg::Tree, IconError> {
    let options = resvg::usvg::Options {
        resources_dir: None,
        ..Default::default()
    };
    resvg::usvg::Tree::from_str(svg, &options).map_err(|error| invalid(error.to_string()))
}

fn render_node_to_canvas(tree: &resvg::usvg::Tree, id: &str) -> Result<RgbaImage, IconError> {
    let node = tree
        .node_by_id(id)
        .ok_or_else(|| invalid(format!("missing renderable layer {id}")))?;
    let mut pixmap = resvg::tiny_skia::Pixmap::new(CANVAS_SIZE, CANVAS_SIZE)
        .ok_or_else(|| invalid("could not allocate layer canvas"))?;
    resvg::render_node(
        node,
        resvg::tiny_skia::Transform::identity(),
        &mut pixmap.as_mut(),
    )
    .ok_or_else(|| invalid(format!("empty layer {id}")))?;
    let layer = RgbaImage::from_raw(CANVAS_SIZE, CANVAS_SIZE, pixmap.take())
        .ok_or_else(|| invalid("invalid rasterized layer"))?;
    if layer.pixels().all(|pixel| pixel[3] == 0) {
        return Err(invalid(format!("layer {id} is empty inside the canvas")));
    }
    Ok(layer)
}

fn invalid(message: impl Into<String>) -> IconError {
    IconError::InvalidSvg(message.into())
}
