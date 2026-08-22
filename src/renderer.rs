use crate::{
    error::IconError,
    model::{Appearance, CANVAS_SIZE},
    svg::{RasterLayer, rasterize_layers},
};
use image::{RgbaImage, imageops};
use std::sync::mpsc;
use wgpu::util::DeviceExt;

const GPU_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;
const MAX_LAYERS: u32 = 5;

/// Apple-style safe zone: the optical artwork area covers roughly 84% of the
/// 1024 px canvas. Artwork smaller than `SAFE_ZONE_KEEP_MIN` is grown toward
/// the target with one shared transform; overflowing artwork is shrunk the
/// same way. Artwork already inside the band keeps its source coordinates.
pub const SAFE_ZONE_FRACTION: f32 = 0.84;
const SAFE_ZONE_TARGET: f32 = 860.0;
const SAFE_ZONE_KEEP_MIN: f32 = SAFE_ZONE_TARGET * 0.92;

/// Bumped whenever composition or material behavior changes so cached icons
/// are rebuilt from their canonical SVGs without another AI request.
pub const RENDERER_REVISION: u32 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderTarget {
    Preview,
    Icon,
}

#[derive(Debug, Clone, Copy)]
pub struct RenderSettings {
    pub appearance: Appearance,
    pub accent: [u8; 3],
    pub dark_background: bool,
    pub pointer: [f32; 2],
    pub layer: Option<usize>,
}

impl Default for RenderSettings {
    fn default() -> Self {
        Self {
            appearance: Appearance::Default,
            accent: [137, 180, 250],
            dark_background: false,
            pointer: [0.0, 0.0],
            layer: None,
        }
    }
}

pub struct GlassRenderer {
    device: wgpu::Device,
    queue: wgpu::Queue,
    pipeline: wgpu::RenderPipeline,
    bind_group_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    icon: Option<GlassIcon>,
}

impl GlassRenderer {
    pub async fn new() -> Result<Self, IconError> {
        let instance = wgpu::Instance::default();
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions::default())
            .await
            .map_err(|error| gpu_error(format!("adapter unavailable: {error}")))?;
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor::default())
            .await
            .map_err(|error| gpu_error(format!("device unavailable: {error}")))?;
        let (bind_group_layout, sampler, pipeline) = create_pipeline(&device);
        Ok(Self {
            device,
            queue,
            pipeline,
            bind_group_layout,
            sampler,
            icon: None,
        })
    }

    pub fn load_svg(&mut self, svg: &str) -> Result<(), IconError> {
        let layers = prepare_canonical_layers(svg)?;
        self.icon = Some(GlassIcon::new(
            &self.device,
            &self.queue,
            &layers,
            &self.bind_group_layout,
            &self.sampler,
        ));
        Ok(())
    }

    pub fn clear(&mut self) {
        self.icon = None;
    }

    pub fn has_preview(&self) -> bool {
        self.icon.is_some()
    }

    pub fn layer_count(&self) -> usize {
        self.icon
            .as_ref()
            .map(|icon| icon.layer_count as usize)
            .unwrap_or_default()
    }

    pub fn render(
        &self,
        width: u32,
        height: u32,
        settings: RenderSettings,
        target: RenderTarget,
    ) -> Result<RgbaImage, IconError> {
        let icon = self
            .icon
            .as_ref()
            .ok_or_else(|| gpu_error("no converted SVG loaded"))?;
        let width = width.max(1);
        let height = height.max(1);
        let bytes_per_row = (width * 4).div_ceil(256) * 256;
        let output_size = u64::from(bytes_per_row) * u64::from(height);
        let output = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("liquid-glass-output"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: GPU_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let output_view = output.create_view(&wgpu::TextureViewDescriptor::default());
        let readback = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("liquid-glass-readback"),
            size: output_size,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        self.queue.write_buffer(
            &icon.uniform,
            0,
            &f32_bytes(&settings.params(target, icon.layer_count)),
        );

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("liquid-glass-render-encoder"),
            });
        {
            let color_attachment = Some(wgpu::RenderPassColorAttachment {
                view: &output_view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                    store: wgpu::StoreOp::Store,
                },
            });
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("liquid-glass-render-pass"),
                color_attachments: &[color_attachment],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &icon.bind_group, &[]);
            pass.draw(0..3, 0..1);
        }
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &output,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &readback,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(bytes_per_row),
                    rows_per_image: Some(height),
                },
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
        self.queue.submit([encoder.finish()]);

        let slice = readback.slice(..);
        let (sender, receiver) = mpsc::sync_channel(1);
        slice.map_async(wgpu::MapMode::Read, move |result| {
            let _ = sender.send(result);
        });
        self.device
            .poll(wgpu::PollType::Wait {
                submission_index: None,
                timeout: None,
            })
            .map_err(|error| gpu_error(format!("GPU polling failed: {error}")))?;
        receiver
            .recv()
            .map_err(|error| gpu_error(format!("GPU readback callback failed: {error}")))?
            .map_err(|error| gpu_error(format!("GPU readback failed: {error}")))?;
        let mapped = slice
            .get_mapped_range()
            .map_err(|error| gpu_error(format!("GPU readback mapping failed: {error}")))?;
        let mut pixels = Vec::with_capacity((width * height * 4) as usize);
        for row in mapped.chunks(bytes_per_row as usize).take(height as usize) {
            pixels.extend_from_slice(&row[..(width * 4) as usize]);
        }
        drop(mapped);
        readback.unmap();
        let mut image = RgbaImage::from_raw(width, height, pixels)
            .ok_or_else(|| gpu_error("GPU returned an invalid RGBA image"))?;
        if target == RenderTarget::Icon {
            apply_canonical_mask(&mut image);
        }
        Ok(image)
    }
}

/// Single canonical rounded-square mask definition. The CPU path below and
/// the WGPU shader (`glass_shader()`) share these exact constants so the
/// preview and the exported icon never disagree about the icon edge.
pub const MASK_RADIUS: f32 = 0.415;
pub const MASK_EXPONENT: f32 = 4.2;
pub const MASK_EDGE_START: f32 = 0.90;
pub const MASK_EDGE_END: f32 = 1.00;

fn mask_distance(uv: [f32; 2]) -> f32 {
    let p_x = (uv[0] - 0.5).abs() / MASK_RADIUS;
    let p_y = (uv[1] - 0.5).abs() / MASK_RADIUS;
    p_x.powf(MASK_EXPONENT) + p_y.powf(MASK_EXPONENT)
}

fn mask_value(distance: f32) -> f32 {
    let t = ((distance - MASK_EDGE_START) / (MASK_EDGE_END - MASK_EDGE_START)).clamp(0.0, 1.0);
    1.0 - t * t * (3.0 - 2.0 * t)
}

/// Apply the canonical icon mask exactly once. Layer textures are never
/// masked; only the final exported image is.
pub fn apply_canonical_mask(image: &mut RgbaImage) {
    let width = image.width() as f32;
    let height = image.height() as f32;
    for (x, y, pixel) in image.enumerate_pixels_mut() {
        let uv_x = (x as f32 + 0.5) / width;
        let uv_y = (y as f32 + 0.5) / height;
        let mask = mask_value(mask_distance([uv_x, uv_y]));
        pixel[3] = (f32::from(pixel[3]) * mask).round() as u8;
    }
}

fn create_pipeline(
    device: &wgpu::Device,
) -> (wgpu::BindGroupLayout, wgpu::Sampler, wgpu::RenderPipeline) {
    let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("liquid-glass-bind-group-layout"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    multisampled: false,
                    view_dimension: wgpu::TextureViewDimension::D2Array,
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 2,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
        ],
    });
    let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("liquid-glass-sampler"),
        mag_filter: wgpu::FilterMode::Linear,
        min_filter: wgpu::FilterMode::Linear,
        address_mode_u: wgpu::AddressMode::ClampToEdge,
        address_mode_v: wgpu::AddressMode::ClampToEdge,
        ..Default::default()
    });
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("liquid-glass-shader"),
        source: wgpu::ShaderSource::Wgsl(glass_shader().into()),
    });
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("liquid-glass-pipeline-layout"),
        bind_group_layouts: &[Some(&bind_group_layout)],
        immediate_size: 0,
    });
    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("liquid-glass-pipeline"),
        layout: Some(&layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vs_main"),
            buffers: &[],
            compilation_options: Default::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: Some("fs_main"),
            targets: &[Some(wgpu::ColorTargetState {
                format: GPU_FORMAT,
                blend: Some(wgpu::BlendState::REPLACE),
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: Default::default(),
        }),
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        multiview_mask: None,
        cache: None,
    });
    (bind_group_layout, sampler, pipeline)
}

struct GlassIcon {
    _texture: wgpu::Texture,
    bind_group: wgpu::BindGroup,
    uniform: wgpu::Buffer,
    layer_count: f32,
}

impl GlassIcon {
    fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        layers: &[RasterLayer],
        layout: &wgpu::BindGroupLayout,
        sampler: &wgpu::Sampler,
    ) -> Self {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("liquid-glass-layers"),
            size: wgpu::Extent3d {
                width: CANVAS_SIZE,
                height: CANVAS_SIZE,
                depth_or_array_layers: MAX_LAYERS,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: GPU_FORMAT,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        for (index, layer) in layers.iter().take(MAX_LAYERS as usize).enumerate() {
            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d {
                        x: 0,
                        y: 0,
                        z: index as u32,
                    },
                    aspect: wgpu::TextureAspect::All,
                },
                layer.image.as_raw(),
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(CANVAS_SIZE * 4),
                    rows_per_image: Some(CANVAS_SIZE),
                },
                wgpu::Extent3d {
                    width: CANVAS_SIZE,
                    height: CANVAS_SIZE,
                    depth_or_array_layers: 1,
                },
            );
        }
        let view = texture.create_view(&wgpu::TextureViewDescriptor {
            label: Some("liquid-glass-layer-view"),
            dimension: Some(wgpu::TextureViewDimension::D2Array),
            base_array_layer: 0,
            array_layer_count: Some(MAX_LAYERS),
            ..Default::default()
        });
        let uniform = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("liquid-glass-uniform"),
            contents: &[0; 48],
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("liquid-glass-bind-group"),
            layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: uniform.as_entire_binding(),
                },
            ],
        });
        Self {
            _texture: texture,
            bind_group,
            uniform,
            layer_count: layers.len().min(MAX_LAYERS as usize) as f32,
        }
    }
}

impl RenderSettings {
    fn params(self, target: RenderTarget, layer_count: f32) -> [f32; 12] {
        let [r, g, b] = self.accent.map(|channel| f32::from(channel) / 255.0);
        [
            r,
            g,
            b,
            1.0,
            appearance_index(self.appearance),
            layer_count,
            if self.dark_background { 1.0 } else { 0.0 },
            if target == RenderTarget::Icon {
                1.0
            } else {
                0.0
            },
            self.pointer[0],
            self.pointer[1],
            self.layer.map(|layer| layer as f32).unwrap_or(-1.0),
            0.0,
        ]
    }
}

pub fn appearance_index(appearance: Appearance) -> f32 {
    match appearance {
        Appearance::Default => 0.0,
        Appearance::Dark => 1.0,
        Appearance::ClearLight => 2.0,
        Appearance::ClearDark => 3.0,
        Appearance::TintedLight => 4.0,
        Appearance::TintedDark => 5.0,
    }
}

/// Rasterize a canonical SVG and run enclosure extraction plus Apple
/// safe-zone fitting. Single entry point shared by the GPU loader,
/// integration tests and tooling.
pub fn prepare_canonical_layers(svg: &str) -> Result<Vec<RasterLayer>, IconError> {
    Ok(fit_layers_to_icon_frame(extract_enclosure(
        rasterize_layers(svg)?,
    )))
}

/// A full-bleed circle or rounded-square sitting in the first foreground
/// slot is an enclosure candidate: its color field moves into the background
/// layer and the final rounded-square shape comes from the renderer mask.
fn extract_enclosure(mut layers: Vec<RasterLayer>) -> Vec<RasterLayer> {
    if layers.len() < 2 || !is_full_bleed_enclosure(&layers[1].image) {
        return layers;
    }
    let enclosure = layers.remove(1);
    // Composite instead of flattening to a single color so gradients and
    // color-field information survive into the background layer.
    imageops::overlay(&mut layers[0].image, &enclosure.image, 0, 0);
    layers
}

const FULL_BLEED_SPAN: u32 = 982;

/// Strict enclosure test: only near-canvas-filling shapes whose edge
/// midpoints are opaque and whose corners stay transparent qualify. Smaller
/// meaningful circles (eyes, badges, logo parts) are kept as artwork.
fn is_full_bleed_enclosure(image: &RgbaImage) -> bool {
    if image.dimensions() != (CANVAS_SIZE, CANVAS_SIZE) {
        return false;
    }
    let edge = CANVAS_SIZE - 1;
    let center = CANVAS_SIZE / 2;
    let midpoints = [
        image.get_pixel(0, center),
        image.get_pixel(edge, center),
        image.get_pixel(center, 0),
        image.get_pixel(center, edge),
    ];
    let corners = [
        image.get_pixel(0, 0),
        image.get_pixel(edge, 0),
        image.get_pixel(0, edge),
        image.get_pixel(edge, edge),
    ];
    if !midpoints.iter().all(|pixel| pixel[3] >= 128) {
        return false;
    }
    if !corners.iter().all(|pixel| pixel[3] <= 32) {
        return false;
    }
    alpha_bounds(image).is_some_and(|(min_x, min_y, max_x, max_y)| {
        max_x - min_x + 1 >= FULL_BLEED_SPAN && max_y - min_y + 1 >= FULL_BLEED_SPAN
    })
}

/// Apple safe-zone fitting. The background never participates; the combined
/// alpha bounds of the artwork decide one shared transform:
/// - inside the keep band and not clipped: keep source coordinates,
///   only translate so the combined center sits at (512, 512);
/// - too small: grow toward ~84% safe zone about that common center;
/// - overflowing (touching a canvas edge): shrink with the same transform
///   instead of enlarging the crop.
///
/// Layers are never scaled against their own bounding boxes.
fn fit_layers_to_icon_frame(layers: Vec<RasterLayer>) -> Vec<RasterLayer> {
    let Some(bounds @ (min_x, min_y, max_x, max_y)) = artwork_bounds(&layers) else {
        return layers;
    };
    let source_width = (max_x - min_x + 1) as f32;
    let source_height = (max_y - min_y + 1) as f32;
    let max_dimension = source_width.max(source_height);
    let touches_edge =
        min_x == 0 || min_y == 0 || max_x == CANVAS_SIZE - 1 || max_y == CANVAS_SIZE - 1;
    let scale = if max_dimension < SAFE_ZONE_KEEP_MIN
        || (touches_edge && max_dimension > SAFE_ZONE_TARGET)
    {
        SAFE_ZONE_TARGET / max_dimension
    } else {
        1.0
    };
    apply_common_transform(layers, bounds, scale)
}

fn apply_common_transform(
    mut layers: Vec<RasterLayer>,
    (min_x, min_y, max_x, max_y): (u32, u32, u32, u32),
    scale: f32,
) -> Vec<RasterLayer> {
    let source_width = max_x - min_x + 1;
    let source_height = max_y - min_y + 1;
    let target_width = ((source_width as f32 * scale).round() as u32).max(1);
    let target_height = ((source_height as f32 * scale).round() as u32).max(1);
    // One transform for every artwork layer: the combined bounding box keeps
    // its internal layout while its center lands on the canvas center.
    let target_x = (((CANVAS_SIZE as f32 - target_width as f32) / 2.0).round() as i64).max(0);
    let target_y = (((CANVAS_SIZE as f32 - target_height as f32) / 2.0).round() as i64).max(0);
    for layer in layers.iter_mut().skip(1) {
        let cropped =
            imageops::crop_imm(&layer.image, min_x, min_y, source_width, source_height).to_image();
        let resized = if (scale - 1.0).abs() < f32::EPSILON {
            cropped
        } else {
            imageops::resize(
                &cropped,
                target_width,
                target_height,
                imageops::FilterType::Lanczos3,
            )
        };
        let mut framed = RgbaImage::new(CANVAS_SIZE, CANVAS_SIZE);
        imageops::overlay(&mut framed, &resized, target_x, target_y);
        layer.image = framed;
    }
    layers
}

fn alpha_bounds(image: &RgbaImage) -> Option<(u32, u32, u32, u32)> {
    let mut min_x = CANVAS_SIZE;
    let mut min_y = CANVAS_SIZE;
    let mut max_x = 0;
    let mut max_y = 0;
    let mut found = false;
    for (x, y, pixel) in image.enumerate_pixels() {
        if pixel[3] <= 8 {
            continue;
        }
        found = true;
        min_x = min_x.min(x);
        min_y = min_y.min(y);
        max_x = max_x.max(x);
        max_y = max_y.max(y);
    }
    found.then_some((min_x, min_y, max_x, max_y))
}

fn artwork_bounds(layers: &[RasterLayer]) -> Option<(u32, u32, u32, u32)> {
    let mut bounds: Option<(u32, u32, u32, u32)> = None;
    for layer in layers.iter().skip(1) {
        let Some((min_x, min_y, max_x, max_y)) = alpha_bounds(&layer.image) else {
            continue;
        };
        bounds = Some(match bounds {
            None => (min_x, min_y, max_x, max_y),
            Some((b_min_x, b_min_y, b_max_x, b_max_y)) => (
                b_min_x.min(min_x),
                b_min_y.min(min_y),
                b_max_x.max(max_x),
                b_max_y.max(max_y),
            ),
        });
    }
    bounds
}

fn f32_bytes(values: &[f32]) -> Vec<u8> {
    values
        .iter()
        .flat_map(|value| value.to_ne_bytes())
        .collect()
}

fn gpu_error(message: impl Into<String>) -> IconError {
    IconError::InvalidImage(format!("Liquid Glass GPU: {}", message.into()))
}

const GLASS_SHADER_TEMPLATE: &str = r#"
struct Params {
    accent: vec4<f32>,
    state: vec4<f32>,
    pointer: vec4<f32>,
}

@group(0) @binding(0) var layers: texture_2d_array<f32>;
@group(0) @binding(1) var layer_sampler: sampler;
@group(0) @binding(2) var<uniform> params: Params;

struct VertexOut {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
}

@vertex
fn vs_main(@builtin(vertex_index) index: u32) -> VertexOut {
    var positions = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>(3.0, -1.0),
        vec2<f32>(-1.0, 3.0),
    );
    let position = positions[index];
    var output: VertexOut;
    output.position = vec4<f32>(position, 0.0, 1.0);
    output.uv = vec2<f32>((position.x + 1.0) * 0.5, (1.0 - position.y) * 0.5);
    return output;
}

fn preview_background(uv: vec2<f32>) -> vec3<f32> {
    let wave = 0.5 + 0.5 * sin((uv.x + uv.y) * 8.0);
    if params.state.z > 0.5 {
        return mix(vec3<f32>(0.025, 0.035, 0.06), vec3<f32>(0.12, 0.16, 0.24), wave);
    }
    return mix(vec3<f32>(0.76, 0.84, 0.96), vec3<f32>(0.96, 0.78, 0.88), wave);
}

fn enclosure_distance(uv: vec2<f32>) -> f32 {
    let p = abs((uv - vec2<f32>(0.5)) / vec2<f32>(@MASK_RADIUS@));
    return pow(p.x, @MASK_EXPONENT@) + pow(p.y, @MASK_EXPONENT@);
}

@fragment
fn fs_main(input: VertexOut) -> @location(0) vec4<f32> {
    let uv = input.uv;
    let source_background = textureSample(layers, layer_sampler, uv, 0);
    var color = source_background.rgb;
    var alpha = source_background.a;
    let environment = preview_background(uv + params.pointer.xy * 0.008);
    if params.pointer.z >= 0.0 {
        let selected_index = i32(params.pointer.z + 0.5);
        let selected = textureSample(layers, layer_sampler, uv, selected_index);
        let selected_alpha = selected.a;
        return vec4(mix(environment, selected.rgb, selected_alpha), 1.0);
    }

    let foreground_count = max(params.state.y - 1.0, 1.0);

    for (var index: i32 = 1; index < 5; index = index + 1) {
        if f32(index) >= params.state.y { break; }
        let z = f32(index) / foreground_count;
        let parallax = params.pointer.xy * z * 0.036;
        let depth_gap = vec2<f32>(0.0, -0.008) * z;
        let sample_uv = uv + parallax + depth_gap;
        let source = textureSample(layers, layer_sampler, sample_uv, index);
        let depth_shadow = source.a * (0.08 + z * 0.08);
        color = color * (1.0 - depth_shadow);

        let edge_x = abs(
            textureSample(layers, layer_sampler, sample_uv + vec2<f32>(0.006, 0.0), index).a
                - textureSample(layers, layer_sampler, sample_uv - vec2<f32>(0.006, 0.0), index).a,
        );
        let edge_y = abs(
            textureSample(layers, layer_sampler, sample_uv + vec2<f32>(0.0, 0.006), index).a
                - textureSample(layers, layer_sampler, sample_uv - vec2<f32>(0.0, 0.006), index).a,
        );
        let edge = clamp(edge_x + edge_y, 0.0, 1.0);
        let specular = vec3<f32>(0.82, 0.90, 1.0) * edge * (0.20 + z * 0.18);
        let refracted = mix(source.rgb, color, 0.08 + z * 0.10);
        let material = refracted + specular;
        let layer_alpha = source.a * (0.72 + z * 0.24);
        color = color * (1.0 - layer_alpha) + material * layer_alpha;
        alpha = alpha + layer_alpha * (1.0 - alpha);
    }

    let luminance = dot(color, vec3<f32>(0.2126, 0.7152, 0.0722));
    let mode = params.state.x;
    let accent_tone = mix(params.accent.rgb * 0.55, params.accent.rgb * 1.15, luminance);
    var artwork = color;
    var glass_mix = 0.10;
    if mode > 0.5 && mode < 1.5 {
        artwork = color * 0.66 + vec3<f32>(0.04, 0.055, 0.08);
        glass_mix = 0.18;
    } else if mode > 1.5 && mode < 2.5 {
        artwork = vec3<f32>(0.82 + luminance * 0.16);
        glass_mix = 0.52;
    } else if mode > 2.5 && mode < 3.5 {
        artwork = vec3<f32>(0.12 + luminance * 0.22);
        glass_mix = 0.58;
    } else if mode > 3.5 {
        artwork = accent_tone;
        glass_mix = select(0.50, 0.56, mode > 4.5);
    }

    let refracted = select(color, environment, params.state.w < 0.5);
    var glass = mix(artwork, refracted, glass_mix);
    // Boundary-based lighting: one symmetric inner glow along the enclosure
    // border plus the per-layer specular from alpha gradients above. No
    // global tilted highlight — symmetric sources stay horizontally centered.
    let rim_distance = enclosure_distance(uv);
    let rim = smoothstep(0.78, 0.92, rim_distance)
        * (1.0 - smoothstep(0.96, 1.02, rim_distance));
    glass = glass + vec3<f32>(rim * 0.10);

    if params.state.w > 0.5 {
        return vec4<f32>(glass, alpha);
    }
    let canvas = environment;
    return vec4<f32>(mix(canvas, glass, alpha), 1.0);
}
"#;

/// Build the shader source with the canonical mask constants injected so the
/// GPU path and `apply_canonical_mask` share one mathematical definition.
fn glass_shader() -> String {
    GLASS_SHADER_TEMPLATE
        .replace("@MASK_RADIUS@", &format!("{MASK_RADIUS}"))
        .replace("@MASK_EXPONENT@", &format!("{MASK_EXPONENT}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layer(id: &str, image: RgbaImage) -> RasterLayer {
        RasterLayer {
            id: id.to_owned(),
            image,
        }
    }

    fn solid(id: &str, fill: [u8; 4]) -> RasterLayer {
        layer(
            id,
            RgbaImage::from_pixel(CANVAS_SIZE, CANVAS_SIZE, image::Rgba(fill)),
        )
    }

    fn rect_layer(id: &str, x0: u32, y0: u32, x1: u32, y1: u32, color: [u8; 4]) -> RasterLayer {
        let mut canvas = RgbaImage::new(CANVAS_SIZE, CANVAS_SIZE);
        for y in y0..y1 {
            for x in x0..x1 {
                canvas.put_pixel(x, y, image::Rgba(color));
            }
        }
        layer(id, canvas)
    }

    /// Vertical-gradient disc used as a full-bleed enclosure candidate.
    fn gradient_disc(id: &str, radius: i64) -> RasterLayer {
        let mut canvas = RgbaImage::new(CANVAS_SIZE, CANVAS_SIZE);
        let center = CANVAS_SIZE as i64 / 2;
        for y in 0..CANVAS_SIZE {
            for x in 0..CANVAS_SIZE {
                let dx = x as i64 - center;
                let dy = y as i64 - center;
                if dx * dx + dy * dy <= radius * radius {
                    let color = if y < CANVAS_SIZE / 2 {
                        [220, 50, 50, 255]
                    } else {
                        [40, 60, 230, 255]
                    };
                    canvas.put_pixel(x, y, image::Rgba(color));
                }
            }
        }
        layer(id, canvas)
    }

    /// Canvas-filling rounded square with transparent corners.
    fn rounded_square(id: &str) -> RasterLayer {
        let mut canvas = RgbaImage::new(CANVAS_SIZE, CANVAS_SIZE);
        let lo = 0i64;
        let hi = CANVAS_SIZE as i64 - 1;
        let radius = 180i64;
        let band = lo + radius;
        for y in lo..=hi {
            for x in lo..=hi {
                let corner = if x < band && y < band {
                    Some((band, band))
                } else if x > hi - radius && y < band {
                    Some((hi - radius, band))
                } else if x < band && y > hi - radius {
                    Some((band, hi - radius))
                } else if x > hi - radius && y > hi - radius {
                    Some((hi - radius, hi - radius))
                } else {
                    None
                };
                let inside = match corner {
                    None => true,
                    Some((cx, cy)) => {
                        let dx = x - cx;
                        let dy = y - cy;
                        dx * dx + dy * dy <= radius * radius
                    }
                };
                if inside {
                    canvas.put_pixel(x as u32, y as u32, image::Rgba([255, 255, 255, 255]));
                }
            }
        }
        layer(id, canvas)
    }

    #[test]
    fn background_never_joins_artwork_bounds_and_keep_band_keeps_coordinates() {
        let background = solid("background", [0, 0, 0, 255]);
        // 760×940 artwork sits inside the keep band, away from every edge:
        // source coordinates survive, only the shared centering translation
        // is applied. If the opaque background joined the measurement the
        // artwork would be shrunk instead.
        let foreground = rect_layer("foreground-1", 140, 40, 900, 980, [255, 255, 255, 255]);
        let fitted = fit_layers_to_icon_frame(vec![background, foreground]);
        let (min_x, min_y, max_x, max_y) = artwork_bounds(&fitted).unwrap();
        assert_eq!(max_x - min_x + 1, 760);
        assert_eq!(max_y - min_y + 1, 940);
        let center_x = f32::from((min_x + max_x + 1) as u16) / 2.0;
        let center_y = f32::from((min_y + max_y + 1) as u16) / 2.0;
        assert!((center_x - 512.0).abs() <= 1.0, "center_x {center_x}");
        assert!((center_y - 512.0).abs() <= 1.0, "center_y {center_y}");
        assert_eq!(fitted[0].image.get_pixel(0, 0)[3], 255);
    }

    #[test]
    fn small_artwork_grows_to_safe_zone_about_common_center() {
        let background = solid("background", [0, 0, 0, 255]);
        let foreground = rect_layer("foreground-1", 462, 462, 562, 562, [255, 255, 255, 255]);
        let fitted = fit_layers_to_icon_frame(vec![background, foreground]);
        let (min_x, min_y, max_x, max_y) = artwork_bounds(&fitted).unwrap();
        let width = max_x - min_x + 1;
        let height = max_y - min_y + 1;
        assert!(
            (width as i32 - SAFE_ZONE_TARGET as i32).abs() <= 4,
            "width {width}"
        );
        assert!(
            (height as i32 - SAFE_ZONE_TARGET as i32).abs() <= 4,
            "height {height}"
        );
        let center_x = f32::from((min_x + max_x + 1) as u16) / 2.0;
        let center_y = f32::from((min_y + max_y + 1) as u16) / 2.0;
        assert!((center_x - 512.0).abs() <= 3.0, "center_x {center_x}");
        assert!((center_y - 512.0).abs() <= 3.0, "center_y {center_y}");
    }

    #[test]
    fn oversized_artwork_shrinks_instead_of_being_cropped() {
        let background = solid("background", [0, 0, 0, 255]);
        let flood = rect_layer(
            "foreground-1",
            0,
            0,
            CANVAS_SIZE,
            CANVAS_SIZE,
            [255, 255, 255, 255],
        );
        let marker = rect_layer("foreground-2", 100, 100, 140, 140, [10, 10, 10, 255]);
        let fitted = fit_layers_to_icon_frame(vec![background, flood, marker]);
        let (min_x, min_y, max_x, _max_y) = artwork_bounds(&fitted).unwrap();
        let width = max_x - min_x + 1;
        assert!(
            (width as i32 - SAFE_ZONE_TARGET as i32).abs() <= 4,
            "width {width}"
        );
        assert!(min_x > 0 && min_y > 0);
        // The marker survives the shared downscale at the expected spot:
        // crop offset (82) plus its scaled position inside the bounding box.
        let scale = SAFE_ZONE_TARGET / CANVAS_SIZE as f32;
        let offset = ((CANVAS_SIZE as f32 - (CANVAS_SIZE as f32 * scale).round()) / 2.0).round();
        let expected = offset + 120.0 * scale;
        let mut sum_x = 0.0f32;
        let mut sum_y = 0.0f32;
        let mut count = 0u32;
        for (x, y, pixel) in fitted[2].image.enumerate_pixels() {
            if pixel[3] > 128 {
                sum_x += x as f32;
                sum_y += y as f32;
                count += 1;
            }
        }
        assert!(count > 0, "marker was cropped away");
        let centroid_x = sum_x / count as f32;
        let centroid_y = sum_y / count as f32;
        assert!(
            (centroid_x - expected).abs() <= 8.0,
            "marker x {centroid_x} vs {expected}"
        );
        assert!(
            (centroid_y - expected).abs() <= 8.0,
            "marker y {centroid_y} vs {expected}"
        );
    }

    #[test]
    fn artwork_layers_share_one_common_transform() {
        let background = solid("background", [0, 0, 0, 255]);
        let back = rect_layer("foreground-1", 200, 200, 400, 400, [255, 255, 255, 255]);
        let front = rect_layer("foreground-2", 600, 600, 800, 800, [255, 0, 0, 255]);
        let fitted = fit_layers_to_icon_frame(vec![background, back, front]);
        let bounds_of = |layer: &RasterLayer| alpha_bounds(&layer.image).unwrap();
        let (a_min_x, _a_min_y, a_max_x, _a_max_y) = bounds_of(&fitted[1]);
        let (b_min_x, _b_min_y, b_max_x, _b_max_y) = bounds_of(&fitted[2]);
        let a_width = a_max_x - a_min_x + 1;
        let b_width = b_max_x - b_min_x + 1;
        // Per-layer fitting would blow each 200 px rect up to the safe zone
        // on its own; the shared transform scales them identically instead.
        assert!((a_width as i32 - 287).abs() <= 3, "a_width {a_width}");
        assert!((b_width as i32 - 287).abs() <= 3, "b_width {b_width}");
        assert!((a_width as i32 - b_width as i32).abs() <= 2);
        let a_center = f32::from((a_min_x + a_max_x + 1) as u16) / 2.0;
        let b_center = f32::from((b_min_x + b_max_x + 1) as u16) / 2.0;
        let gap = b_center - a_center;
        let expected_gap = 400.0 * (SAFE_ZONE_TARGET / 600.0);
        assert!((gap - expected_gap).abs() <= 6.0, "gap {gap}");
    }

    #[test]
    fn full_bleed_first_circle_becomes_the_background_color_field() {
        let background = solid("background", [30, 160, 80, 255]);
        let circle = gradient_disc("foreground-1", 512);
        let logo = rect_layer("foreground-2", 200, 200, 300, 300, [255, 255, 255, 255]);
        let layers = extract_enclosure(vec![background, circle, logo]);
        assert_eq!(layers.len(), 2);
        assert_eq!(layers[1].id, "foreground-2");
        // The gradient survives into the background instead of being
        // flattened to one solid center color.
        let top = layers[0].image.get_pixel(512, 80);
        let bottom = layers[0].image.get_pixel(512, 944);
        assert!(top[0] > top[1] && top[0] > top[2], "top {top:?}");
        assert!(
            bottom[2] > bottom[0] && bottom[2] > bottom[1],
            "bottom {bottom:?}"
        );
        assert_ne!(top.0, bottom.0);
        assert_eq!(layers[1].image.get_pixel(250, 250)[3], 255);
    }

    #[test]
    fn enclosure_detection_stays_strict() {
        let background = solid("background", [30, 160, 80, 255]);
        // A circle in a later slot is meaningful artwork and stays.
        let kept = extract_enclosure(vec![
            background.clone(),
            rect_layer("foreground-1", 400, 400, 500, 500, [255, 255, 255, 255]),
            gradient_disc("foreground-2", 512),
            rect_layer("foreground-3", 700, 700, 760, 760, [10, 10, 10, 255]),
        ]);
        assert_eq!(kept.len(), 4);
        assert_eq!(kept[2].image.get_pixel(0, 0)[3], 0);

        // Small circles never qualify as enclosures.
        let small = extract_enclosure(vec![background.clone(), gradient_disc("foreground-1", 300)]);
        assert_eq!(small.len(), 2);
        assert_eq!(small[1].image.get_pixel(0, 0)[3], 0);

        // A full-bleed rounded square is also an enclosure.
        let rounded = extract_enclosure(vec![background.clone(), rounded_square("foreground-1")]);
        assert_eq!(rounded.len(), 1);
        assert_eq!(rounded[0].image.get_pixel(512, 60).0, [255, 255, 255, 255]);
        // Corners of the rounded square stay transparent over the old fill.
        assert_eq!(rounded[0].image.get_pixel(2, 2).0, [30, 160, 80, 255]);
    }

    #[test]
    fn canonical_mask_is_centered_feathered_and_shared_with_shader() {
        assert_eq!(mask_value(mask_distance([0.5, 0.5])), 1.0);
        assert_eq!(mask_value(mask_distance([0.0, 0.0])), 0.0);
        assert_eq!(
            mask_value(mask_distance([0.5 - MASK_RADIUS * 0.85, 0.5])),
            1.0
        );
        let mid_edge = mask_value(mask_distance([0.5 + MASK_RADIUS * 0.99, 0.5]));
        assert!(mid_edge > 0.0 && mid_edge < 1.0, "edge feather {mid_edge}");
        assert_eq!(
            mask_value(mask_distance([0.5 + MASK_RADIUS * 1.02, 0.5])),
            0.0
        );

        let mut image = RgbaImage::from_pixel(128, 128, image::Rgba([90, 90, 90, 255]));
        apply_canonical_mask(&mut image);
        assert_eq!(image.get_pixel(64, 64)[3], 255);
        assert_eq!(image.get_pixel(0, 0)[3], 0);
        assert!(image.get_pixel(0, 64)[3] < 10);

        let shader = glass_shader();
        assert!(shader.contains("0.415"), "mask radius missing");
        assert!(shader.contains("4.2"), "mask exponent missing");
        assert!(!shader.contains("@MASK_"), "unresolved placeholder");
    }

    #[tokio::test]
    async fn glass_shader_builds_on_an_available_adapter() {
        let instance = wgpu::Instance::default();
        let Ok(adapter) = instance
            .request_adapter(&wgpu::RequestAdapterOptions::default())
            .await
        else {
            return;
        };
        let (device, _queue) = adapter
            .request_device(&wgpu::DeviceDescriptor::default())
            .await
            .unwrap();
        let error_scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
        let _ = create_pipeline(&device);
        assert!(error_scope.pop().await.is_none());
    }

    #[tokio::test]
    async fn rendered_icon_contains_visible_layer_pixels() {
        let mut renderer = match GlassRenderer::new().await {
            Ok(renderer) => renderer,
            Err(_) => return,
        };
        renderer
            .load_svg(
                r##"<svg xmlns="http://www.w3.org/2000/svg" width="1024" height="1024" viewBox="0 0 1024 1024">
<g id="background"><rect width="1024" height="1024" fill="#203050"/></g>
<g id="foreground-1"><circle cx="512" cy="512" r="240" fill="#ff2030"/></g>
</svg>"##,
            )
            .unwrap();
        let image = renderer
            .render(128, 128, RenderSettings::default(), RenderTarget::Icon)
            .unwrap();
        assert!(image.get_pixel(64, 64)[3] > 0);
        assert_eq!(image.get_pixel(0, 0)[3], 0);
        assert_eq!(image.get_pixel(127, 0)[3], 0);
        assert_eq!(image.get_pixel(0, 127)[3], 0);
        assert_eq!(image.get_pixel(127, 127)[3], 0);

        let selected = renderer
            .render(
                128,
                128,
                RenderSettings {
                    layer: Some(1),
                    ..RenderSettings::default()
                },
                RenderTarget::Preview,
            )
            .unwrap();
        assert!(selected.get_pixel(64, 64)[0] > 200);
        assert!(selected.get_pixel(64, 64)[1] < 100);
    }

    #[tokio::test]
    async fn symmetric_source_stays_horizontally_centered() {
        let Ok(mut renderer) = GlassRenderer::new().await else {
            return;
        };
        renderer
            .load_svg(
                r##"<svg xmlns="http://www.w3.org/2000/svg" width="1024" height="1024" viewBox="0 0 1024 1024">
<g id="background"><rect width="1024" height="1024" fill="#203050"/></g>
<g id="foreground-1"><circle cx="512" cy="512" r="300" fill="#5865F2"/></g>
<g id="foreground-2"><circle cx="392" cy="430" r="48" fill="#ffffff"/><circle cx="632" cy="430" r="48" fill="#ffffff"/></g>
<g id="foreground-3"><path d="M392,640 Q512,752 632,640" stroke="#ffffff" stroke-width="36" fill="none"/></g>
</svg>"##,
            )
            .unwrap();
        let image = renderer
            .render(256, 256, RenderSettings::default(), RenderTarget::Icon)
            .unwrap();
        let mut left = 0u64;
        let mut right = 0u64;
        let mut weighted_x = 0u64;
        let mut total = 0u64;
        for (x, _y, pixel) in image.enumerate_pixels() {
            let alpha = u64::from(pixel[3]);
            if x < 128 {
                left += alpha;
            } else {
                right += alpha;
            }
            weighted_x += u64::from(x) * alpha;
            total += alpha;
        }
        assert!(total > 0);
        let asymmetry = left.abs_diff(right) as f64 / (left + right) as f64;
        assert!(
            asymmetry <= 0.02,
            "horizontal asymmetry {asymmetry} (left {left}, right {right})"
        );
        let centroid_x = weighted_x as f64 / total as f64;
        assert!((centroid_x - 127.5).abs() <= 2.0, "centroid_x {centroid_x}");
    }

    #[tokio::test]
    async fn every_layer_view_renders_without_crashing() {
        use std::fs;
        let Ok(mut renderer) = GlassRenderer::new().await else {
            return;
        };
        let discord_svg = std::env::var("HOME")
            .ok()
            .map(|home| home + "/.local/share/liquid-glass-icon/out/apps/discord/icon.svg");
        let svg = discord_svg
            .and_then(|path| fs::read_to_string(path).ok())
            .expect("no svg fixture available");
        renderer.load_svg(&svg).unwrap();
        let count = renderer.layer_count();
        assert!(count >= 1);
        for layer in 0..count {
            let settings = RenderSettings {
                layer: Some(layer),
                ..RenderSettings::default()
            };
            let image = renderer
                .render(520, 520, settings, RenderTarget::Preview)
                .unwrap();
            assert_eq!(image.dimensions(), (520, 520));
        }
        let composite = renderer
            .render(520, 520, RenderSettings::default(), RenderTarget::Icon)
            .unwrap();
        assert_eq!(composite.dimensions(), (520, 520));
    }
}
