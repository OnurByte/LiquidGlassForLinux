use crate::{
    error::IconError,
    model::{
        AppearanceAnnotation, CANVAS_SIZE, GroupMode, IconDocument, LayerArtifact, MaterialGroup,
        MaterialSettings, SpecularMode,
    },
};
use image::RgbaImage;
use roxmltree::{Document, Node};

#[derive(Debug, Clone)]
pub struct RasterLayer {
    pub id: String,
    pub image: RgbaImage,
}

#[derive(Debug, Clone)]
pub struct RasterGroup {
    pub group: MaterialGroup,
    pub layers: Vec<RasterLayer>,
}

#[derive(Debug, Clone)]
pub struct RasterDocument {
    pub background: RasterLayer,
    pub groups: Vec<RasterGroup>,
}

pub fn validate_svg(svg: &str) -> Result<Vec<LayerArtifact>, IconError> {
    let document = validate_icon_document(svg)?;
    let tree = parse_tree(svg)?;
    let background = render_node_to_canvas(&tree, &document.background.id)?;
    if background.pixels().any(|pixel| pixel[3] != 255) {
        return Err(invalid(
            "background layer must cover the full canvas and be opaque",
        ));
    }
    Ok(document.layers())
}

pub fn validate_svg_structure(svg: &str) -> Result<Vec<LayerArtifact>, IconError> {
    Ok(validate_icon_document(svg)?.layers())
}

/// Parse both the v4 nested document and the v2/v3 flat foreground layout.
/// Legacy foreground groups become one automatic Individual material group.
pub fn validate_icon_document(svg: &str) -> Result<IconDocument, IconError> {
    let document =
        Document::parse(svg).map_err(|error| IconError::InvalidSvg(error.to_string()))?;
    let root = document.root_element();
    if root.tag_name().name() != "svg" {
        return Err(invalid("root element must be svg"));
    }
    validate_view_box(root)?;
    validate_untrusted_svg(&root)?;
    let groups = root
        .children()
        .filter(|node| node.is_element() && node.tag_name().name() != "defs")
        .collect::<Vec<_>>();
    if !(2..=5).contains(&groups.len()) || groups.iter().any(|node| node.tag_name().name() != "g") {
        return Err(invalid(
            "SVG must contain a background plus one to four material groups",
        ));
    }
    if groups[0].attribute("id") != Some("background") {
        return Err(invalid("layer 0 must be named background"));
    }
    let background = LayerArtifact {
        id: "background".to_owned(),
        z_index: 0,
    };
    let nested = groups
        .get(1)
        .and_then(|node| node.attribute("id"))
        .is_some_and(|id| id.starts_with("group-"));
    let material_groups = if nested {
        parse_nested_groups(&groups[1..])?
    } else {
        parse_legacy_groups(&groups[1..])?
    };
    let tree = parse_tree(svg)?;
    if !tree.filters().is_empty() {
        return Err(invalid("filters are not allowed"));
    }
    Ok(IconDocument {
        background,
        groups: material_groups,
    })
}

pub fn rasterize_document(svg: &str) -> Result<RasterDocument, IconError> {
    let document = validate_icon_document(svg)?;
    let tree = parse_tree(svg)?;
    let background = RasterLayer {
        id: document.background.id.clone(),
        image: render_node_to_canvas(&tree, &document.background.id)?,
    };
    if background.image.pixels().any(|pixel| pixel[3] != 255) {
        return Err(invalid(
            "background layer must cover the full canvas and be opaque",
        ));
    }
    let groups = document
        .groups
        .into_iter()
        .map(|group| {
            let layers = group
                .layers
                .iter()
                .map(|layer| {
                    Ok(RasterLayer {
                        id: layer.id.clone(),
                        image: render_node_to_canvas(&tree, &layer.id)?,
                    })
                })
                .collect::<Result<Vec<_>, IconError>>()?;
            Ok(RasterGroup { group, layers })
        })
        .collect::<Result<Vec<_>, IconError>>()?;
    Ok(RasterDocument { background, groups })
}

/// Flat callers continue to receive source layers in their original SVG
/// coordinates. New callers should prefer `rasterize_document`.
pub fn rasterize_layers(svg: &str) -> Result<Vec<RasterLayer>, IconError> {
    let document = rasterize_document(svg)?;
    let mut layers = vec![document.background];
    for group in document.groups {
        layers.extend(group.layers);
    }
    Ok(layers)
}

fn validate_view_box(root: Node<'_, '_>) -> Result<(), IconError> {
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
    Ok(())
}

fn validate_untrusted_svg(root: &Node<'_, '_>) -> Result<(), IconError> {
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
    Ok(())
}

fn parse_legacy_groups(groups: &[Node<'_, '_>]) -> Result<Vec<MaterialGroup>, IconError> {
    groups
        .iter()
        .enumerate()
        .map(|(offset, group)| {
            let index = offset + 1;
            let expected = format!("foreground-{index}");
            if group.attribute("id") != Some(expected.as_str()) {
                return Err(invalid(format!("layer {index} must be named {expected}")));
            }
            let layer = LayerArtifact {
                id: expected.clone(),
                z_index: index as u8,
            };
            Ok(MaterialGroup {
                id: expected,
                z_index: index as u8,
                layers: vec![layer],
                material: MaterialSettings::default(),
                dark: AppearanceAnnotation::default(),
                mono: AppearanceAnnotation::default(),
            })
        })
        .collect()
}

fn parse_nested_groups(groups: &[Node<'_, '_>]) -> Result<Vec<MaterialGroup>, IconError> {
    groups
        .iter()
        .enumerate()
        .map(|(offset, group)| {
            let index = offset + 1;
            let expected = format!("group-{index}");
            if group.attribute("id") != Some(expected.as_str()) {
                return Err(invalid(format!(
                    "material group {index} must be named {expected}"
                )));
            }
            let children = group
                .children()
                .filter(|child| child.is_element())
                .collect::<Vec<_>>();
            if children.is_empty()
                || children.len() > 4
                || children.iter().any(|child| child.tag_name().name() != "g")
            {
                return Err(invalid(format!(
                    "{expected} must contain one to four named child layers"
                )));
            }
            let layers = children
                .iter()
                .enumerate()
                .map(|(child_offset, child)| {
                    let child_index = child_offset + 1;
                    let id = format!("layer-{index}-{child_index}");
                    if child.attribute("id") != Some(id.as_str()) {
                        return Err(invalid(format!(
                            "{expected} child {child_index} must be named {id}"
                        )));
                    }
                    Ok(LayerArtifact {
                        id,
                        z_index: child_index as u8,
                    })
                })
                .collect::<Result<Vec<_>, IconError>>()?;
            Ok(MaterialGroup {
                id: expected,
                z_index: index as u8,
                layers,
                material: material_settings(group)?,
                dark: annotation(group, "dark")?,
                mono: annotation(group, "mono")?,
            })
        })
        .collect()
}

fn material_settings(group: &Node<'_, '_>) -> Result<MaterialSettings, IconError> {
    let mode = match group.attribute("data-liquid-mode").unwrap_or("individual") {
        "individual" => GroupMode::Individual,
        "combined" => GroupMode::Combined,
        other => return Err(invalid(format!("invalid data-liquid-mode {other}"))),
    };
    let specular = match group
        .attribute("data-liquid-specular")
        .unwrap_or("automatic")
    {
        "off" => SpecularMode::Off,
        "automatic" => SpecularMode::Automatic,
        "inside" => SpecularMode::Inside,
        "outside" => SpecularMode::Outside,
        other => return Err(invalid(format!("invalid data-liquid-specular {other}"))),
    };
    Ok(MaterialSettings {
        effects_enabled: bool_attribute(group, "data-liquid-effects", true)?,
        mode,
        specular,
        blur: unit_attribute(group, "data-liquid-blur", 0.0)?,
        refraction: [
            unit_attribute(group, "data-liquid-refraction-x", 0.5)?,
            unit_attribute(group, "data-liquid-refraction-y", 0.5)?,
        ],
        translucency: unit_attribute(group, "data-liquid-translucency", 0.5)?,
        shadow: unit_attribute(group, "data-liquid-shadow", 0.5)?,
    })
}

fn annotation(group: &Node<'_, '_>, appearance: &str) -> Result<AppearanceAnnotation, IconError> {
    let opacity_name = format!("data-liquid-{appearance}-opacity");
    let effects_name = format!("data-liquid-{appearance}-effects");
    let opacity = group
        .attribute(opacity_name.as_str())
        .map(|value| parse_unit(value, "opacity"))
        .transpose()?;
    let effects_enabled = group
        .attribute(effects_name.as_str())
        .map(|value| parse_bool(value, "effects"))
        .transpose()?;
    Ok(AppearanceAnnotation {
        opacity,
        effects_enabled,
    })
}

fn bool_attribute(group: &Node<'_, '_>, name: &str, default: bool) -> Result<bool, IconError> {
    group
        .attribute(name)
        .map(|value| parse_bool(value, name))
        .transpose()
        .map(|value| value.unwrap_or(default))
}

fn unit_attribute(group: &Node<'_, '_>, name: &str, default: f32) -> Result<f32, IconError> {
    group
        .attribute(name)
        .map(|value| parse_unit(value, name))
        .transpose()
        .map(|value| value.unwrap_or(default))
}

fn parse_bool(value: &str, field: &str) -> Result<bool, IconError> {
    match value {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => Err(invalid(format!("invalid {field}"))),
    }
}

fn parse_unit(value: &str, field: &str) -> Result<f32, IconError> {
    let parsed = value
        .parse::<f32>()
        .map_err(|_| invalid(format!("invalid {field}")))?;
    if !(0.0..=1.0).contains(&parsed) {
        return Err(invalid(format!("{field} must be between 0 and 1")));
    }
    Ok(parsed)
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
    let bbox = node
        .abs_layer_bounding_box()
        .ok_or_else(|| invalid(format!("empty layer {id}")))?;
    resvg::render_node(
        node,
        resvg::tiny_skia::Transform::from_translate(bbox.x(), bbox.y()),
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
