use crate::{
    error::IconError,
    model::{
        Appearance, AppearanceAnnotation, CANVAS_SIZE, GroupMode, MaterialSettings, SpecularMode,
    },
    svg::{RasterLayer, rasterize_document, rasterize_layers},
};
use image::{Rgba, RgbaImage, imageops};
use roxmltree::Document;
use std::sync::mpsc;
use wgpu::util::DeviceExt;

const GPU_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;
/// One opaque background plus at most four Icon Composer groups with four
/// independent child layers each.
pub const MAX_SURFACES: usize = 16;
const MAX_TEXTURE_LAYERS: u32 = MAX_SURFACES as u32 + 1;

/// Bumped whenever composition or material behavior changes so cached icons
/// are rebuilt from their canonical SVGs without another AI request.
pub const RENDERER_REVISION: u32 = 9;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderTarget {
    Preview,
    Icon,
}

#[derive(Debug, Clone, Copy)]
pub struct RenderSettings {
    pub appearance: Appearance,
    pub accent: [u8; 3],
    /// Global multiplier for every material surface, independent from the
    /// opaque source background.
    pub foreground_opacity: f32,
    /// Replaces only the source colour field at runtime; the canonical SVG
    /// remains the portable source of truth.
    pub background: Option<[u8; 3]>,
    /// Local, per-rendered-surface preferences. These never mutate the
    /// canonical SVG or ask the provider to regenerate an icon.
    pub surface_overrides: [SurfaceOverride; MAX_SURFACES],
    pub dark_background: bool,
    pub pointer: [f32; 2],
    /// Preview-only perspective response to pointer movement.
    pub tilt: bool,
    pub layer: Option<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SurfaceOverride {
    #[serde(default)]
    pub color: Option<[u8; 3]>,
    #[serde(default = "default_surface_opacity")]
    pub opacity: f32,
    /// One-based depth plane assigned locally by the layer inspector. Absent
    /// keeps the canonical source order.
    #[serde(default)]
    pub plane: Option<u8>,
}

const fn default_surface_opacity() -> f32 {
    1.0
}

impl Default for SurfaceOverride {
    fn default() -> Self {
        Self {
            color: None,
            opacity: default_surface_opacity(),
            plane: None,
        }
    }
}

impl Default for RenderSettings {
    fn default() -> Self {
        Self {
            appearance: Appearance::Default,
            accent: [137, 180, 250],
            foreground_opacity: 1.0,
            background: None,
            surface_overrides: [SurfaceOverride {
                color: None,
                opacity: 1.0,
                plane: None,
            }; MAX_SURFACES],
            dark_background: false,
            pointer: [0.0, 0.0],
            tilt: false,
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
        let document = prepare_canonical_document(svg)?;
        self.icon = Some(GlassIcon::new(
            &self.device,
            &self.queue,
            document,
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
            .map(|icon| icon.surface_settings.len() + 1)
            .unwrap_or_default()
    }

    pub fn inspect_labels(&self) -> Vec<String> {
        self.icon
            .as_ref()
            .map(|icon| {
                std::iter::once("Background".to_owned())
                    .chain(icon.surface_labels.iter().cloned())
                    .collect()
            })
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
            &f32_bytes(&settings.params(target, &icon.surface_settings, icon.background_luminance)),
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
/// The exported PNG owns the whole canvas. Safe-zone fitting applies to
/// foreground artwork, never to the enclosure itself; otherwise Linux
/// launchers add a second visible padding ring and the icon looks smaller
/// than its native neighbours.
pub const MASK_RADIUS: f32 = 0.5;
pub const MASK_EXPONENT: f32 = 4.2;
// Keep the straight midpoint of the icon fully opaque. Feathering it at 50%
// made otherwise full-bleed Hicolor icons look inset beside native launchers.
pub const MASK_EDGE_START: f32 = 1.0;
pub const MASK_EDGE_END: f32 = 1.075;

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
    surface_settings: Vec<SurfaceSettings>,
    surface_labels: Vec<String>,
    background_luminance: f32,
}

#[derive(Debug, Clone, Copy)]
struct SurfaceSettings {
    material: MaterialSettings,
    dark: AppearanceAnnotation,
    mono: AppearanceAnnotation,
}

struct MaterialSurface {
    image: RgbaImage,
    label: String,
    settings: SurfaceSettings,
}

struct PreparedDocument {
    background: RgbaImage,
    surfaces: Vec<MaterialSurface>,
}

impl GlassIcon {
    fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        document: PreparedDocument,
        layout: &wgpu::BindGroupLayout,
        sampler: &wgpu::Sampler,
    ) -> Self {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("liquid-glass-layers"),
            size: wgpu::Extent3d {
                width: CANVAS_SIZE,
                height: CANVAS_SIZE,
                depth_or_array_layers: MAX_TEXTURE_LAYERS,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: GPU_FORMAT,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            document.background.as_raw(),
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
        for (index, surface) in document.surfaces.iter().take(MAX_SURFACES).enumerate() {
            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d {
                        x: 0,
                        y: 0,
                        z: index as u32 + 1,
                    },
                    aspect: wgpu::TextureAspect::All,
                },
                surface.image.as_raw(),
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
            array_layer_count: Some(MAX_TEXTURE_LAYERS),
            ..Default::default()
        });
        let uniform = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("liquid-glass-uniform"),
            contents: &[0; 80 + MAX_SURFACES * 64],
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
            surface_settings: document
                .surfaces
                .iter()
                .map(|surface| surface.settings)
                .collect(),
            surface_labels: document
                .surfaces
                .iter()
                .map(|surface| surface.label.clone())
                .collect(),
            background_luminance: average_linear_luminance(&document.background),
        }
    }
}

impl RenderSettings {
    fn params(
        self,
        target: RenderTarget,
        surfaces: &[SurfaceSettings],
        background_luminance: f32,
    ) -> Vec<f32> {
        let [r, g, b] = self.accent.map(|channel| f32::from(channel) / 255.0);
        let pointer = if target == RenderTarget::Icon {
            [0.0, 0.0]
        } else {
            self.pointer
        };
        let [background_r, background_g, background_b] = self
            .background
            .unwrap_or([0, 0, 0])
            .map(srgb_channel_to_linear);
        let mut values = vec![
            r,
            g,
            b,
            self.foreground_opacity.clamp(0.20, 1.50),
            appearance_index(self.appearance),
            surfaces.len() as f32 + 1.0,
            if self.dark_background { 1.0 } else { 0.0 },
            if target == RenderTarget::Icon {
                1.0
            } else {
                0.0
            },
            pointer[0],
            pointer[1],
            self.layer.map(|layer| layer as f32).unwrap_or(-1.0),
            if target == RenderTarget::Preview && self.tilt {
                1.0
            } else {
                0.0
            },
            background_r,
            background_g,
            background_b,
            if self.background.is_some() { 1.0 } else { 0.0 },
            background_luminance.max(0.0001),
            0.0,
            0.0,
            0.0,
        ];

        let mut material_values = Vec::with_capacity(MAX_SURFACES * 4);
        let mut optical_values = Vec::with_capacity(MAX_SURFACES * 4);
        let mut annotation_values = Vec::with_capacity(MAX_SURFACES * 4);
        let mut override_values = Vec::with_capacity(MAX_SURFACES * 4);
        for index in 0..MAX_SURFACES {
            let settings = surfaces.get(index).copied().unwrap_or(SurfaceSettings {
                material: MaterialSettings {
                    effects_enabled: false,
                    ..MaterialSettings::default()
                },
                dark: AppearanceAnnotation::default(),
                mono: AppearanceAnnotation::default(),
            });
            let material = settings.material;
            let surface_override = self.surface_overrides[index];
            material_values.extend([
                if material.effects_enabled { 1.0 } else { 0.0 },
                specular_index(material.specular),
                material.blur.clamp(0.0, 1.0),
                material.translucency.clamp(0.0, 1.0),
            ]);
            optical_values.extend([
                material.refraction[0].clamp(0.0, 1.0),
                material.refraction[1].clamp(0.0, 1.0),
                material.shadow.clamp(0.0, 1.0),
                surface_override.plane.map(f32::from).unwrap_or(-1.0),
            ]);
            annotation_values.extend([
                settings.dark.opacity.unwrap_or(-1.0),
                settings.mono.opacity.unwrap_or(-1.0),
                override_index(settings.dark.effects_enabled),
                override_index(settings.mono.effects_enabled),
            ]);
            let color = surface_override
                .color
                .map(|color| color.map(srgb_channel_to_linear))
                .unwrap_or([-1.0, -1.0, -1.0]);
            override_values.extend([
                color[0],
                color[1],
                color[2],
                surface_override.opacity.clamp(0.0, 1.0),
            ]);
        }
        // WGSL array fields are structure-of-arrays, not one material/optical/
        // annotation tuple per surface. Keeping these blocks contiguous is
        // what makes each material surface read its own settings.
        values.extend(material_values);
        values.extend(optical_values);
        values.extend(annotation_values);
        values.extend(override_values);
        values
    }
}

fn srgb_channel_to_linear(channel: u8) -> f32 {
    let channel = f32::from(channel) / 255.0;
    if channel <= 0.04045 {
        channel / 12.92
    } else {
        ((channel + 0.055) / 1.055).powf(2.4)
    }
}

fn average_linear_luminance(image: &RgbaImage) -> f32 {
    let mut total = 0.0;
    let mut weight = 0.0;
    for pixel in image.pixels() {
        let alpha = f32::from(pixel[3]) / 255.0;
        if alpha == 0.0 {
            continue;
        }
        total += alpha
            * (0.2126 * srgb_channel_to_linear(pixel[0])
                + 0.7152 * srgb_channel_to_linear(pixel[1])
                + 0.0722 * srgb_channel_to_linear(pixel[2]));
        weight += alpha;
    }
    (total / weight.max(1.0)).max(0.0001)
}

fn specular_index(mode: SpecularMode) -> f32 {
    match mode {
        SpecularMode::Off => 0.0,
        SpecularMode::Automatic => 1.0,
        SpecularMode::Inside => 2.0,
        SpecularMode::Outside => 3.0,
    }
}

fn override_index(value: Option<bool>) -> f32 {
    match value {
        None => -1.0,
        Some(false) => 0.0,
        Some(true) => 1.0,
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

/// Rasterize flat source layers in their canonical 1024-grid coordinates.
/// The final system shape belongs to the renderer; source layers are never
/// rescaled, re-centred, or converted into a pre-masked enclosure.
pub fn prepare_canonical_layers(svg: &str) -> Result<Vec<RasterLayer>, IconError> {
    rasterize_layers(svg)
}

fn prepare_canonical_document(svg: &str) -> Result<PreparedDocument, IconError> {
    let document = rasterize_document(svg)?;
    let background = document.background.image;
    let may_contain_legacy_cutouts = source_may_paint_background_colour(svg, &background);
    let mut surfaces = Vec::new();
    for group in document.groups {
        let group_label = group
            .group
            .id
            .strip_prefix("group-")
            .map(|index| format!("Group {index}"))
            .unwrap_or_else(|| format!("Group {}", group.group.z_index));
        let settings = SurfaceSettings {
            material: group.group.material,
            dark: group.group.dark,
            mono: group.group.mono,
        };
        match group.group.material.mode {
            GroupMode::Individual => {
                for (index, layer) in group.layers.into_iter().enumerate() {
                    let mut image = layer.image;
                    if may_contain_legacy_cutouts {
                        remove_background_colored_cutouts(
                            &mut image,
                            &background,
                            &mut surfaces,
                            None,
                        );
                    }
                    surfaces.push(MaterialSurface {
                        image,
                        label: format!("{group_label} / Layer {}", index + 1),
                        settings,
                    });
                }
            }
            GroupMode::Combined => {
                let mut image = RgbaImage::new(CANVAS_SIZE, CANVAS_SIZE);
                for layer in group.layers {
                    let mut layer = layer.image;
                    if may_contain_legacy_cutouts {
                        remove_background_colored_cutouts(
                            &mut layer,
                            &background,
                            &mut surfaces,
                            Some(&mut image),
                        );
                    }
                    imageops::overlay(&mut image, &layer, 0, 0);
                }
                surfaces.push(MaterialSurface {
                    image,
                    label: format!("{group_label} (Combined)"),
                    settings,
                });
            }
        }
    }
    if surfaces.is_empty() || surfaces.len() > MAX_SURFACES {
        return Err(gpu_error(
            "icon must resolve to one to sixteen material surfaces",
        ));
    }
    Ok(PreparedDocument {
        background,
        surfaces,
    })
}

fn source_may_paint_background_colour(svg: &str, background: &RgbaImage) -> bool {
    let background = background.get_pixel(0, 0);
    if background[3] != 255 {
        return false;
    }
    let Ok(document) = Document::parse(svg) else {
        return false;
    };
    document
        .descendants()
        .filter(|node| {
            node.is_element()
                && !node
                    .ancestors()
                    .any(|ancestor| ancestor.attribute("id") == Some("background"))
        })
        .flat_map(|node| [node.attribute("fill"), node.attribute("stroke")])
        .flatten()
        .any(|paint| paint_matches_source_background(paint, *background))
}

fn paint_matches_source_background(paint: &str, background: Rgba<u8>) -> bool {
    parse_svg_paint_rgb(paint)
        .is_some_and(|rgb| rgb == [background[0], background[1], background[2]])
}

fn parse_svg_paint_rgb(paint: &str) -> Option<[u8; 3]> {
    let paint = paint.trim();
    if let Some(hex) = paint.strip_prefix('#') {
        if hex.len() == 3 {
            let mut rgb = [0; 3];
            for (index, channel) in hex.bytes().enumerate() {
                let value = char::from(channel).to_digit(16)? as u8;
                rgb[index] = value * 17;
            }
            return Some(rgb);
        }
        if hex.len() == 6 {
            return Some([
                u8::from_str_radix(&hex[0..2], 16).ok()?,
                u8::from_str_radix(&hex[2..4], 16).ok()?,
                u8::from_str_radix(&hex[4..6], 16).ok()?,
            ]);
        }
    }
    let values = paint
        .strip_prefix("rgb(")
        .or_else(|| paint.strip_prefix("rgba("))?
        .strip_suffix(')')?;
    let mut channels = values
        .split(|character: char| character == ',' || character.is_ascii_whitespace())
        .filter(|value| !value.is_empty());
    Some([
        channels.next()?.parse().ok()?,
        channels.next()?.parse().ok()?,
        channels.next()?.parse().ok()?,
    ])
}

/// Old generated SVGs occasionally paint negative space (for example,
/// Discord's eyes) using the original background colour in a higher layer.
/// Those pixels are semantic cutouts, so punch them through every material
/// surface beneath them and leave the runtime background visible.
fn remove_background_colored_cutouts(
    layer: &mut RgbaImage,
    background: &RgbaImage,
    surfaces: &mut [MaterialSurface],
    mut combined_surface: Option<&mut RgbaImage>,
) {
    for y in 0..CANVAS_SIZE {
        for x in 0..CANVAS_SIZE {
            let cutout = *layer.get_pixel(x, y);
            if !matches_source_background(cutout, *background.get_pixel(x, y)) {
                continue;
            }
            let covered_by_surface = surfaces
                .iter()
                .any(|surface| surface.image.get_pixel(x, y)[3] > 0)
                || combined_surface
                    .as_deref()
                    .is_some_and(|surface| surface.get_pixel(x, y)[3] > 0);
            if !covered_by_surface {
                continue;
            }
            for surface in surfaces.iter_mut() {
                erase_with_alpha(surface.image.get_pixel_mut(x, y), cutout[3]);
            }
            if let Some(surface) = combined_surface.as_deref_mut() {
                erase_with_alpha(surface.get_pixel_mut(x, y), cutout[3]);
            }
            *layer.get_pixel_mut(x, y) = Rgba([0, 0, 0, 0]);
        }
    }
}

fn matches_source_background(pixel: Rgba<u8>, background: Rgba<u8>) -> bool {
    let alpha = u16::from(pixel[3]);
    if alpha == 0 || background[3] < 250 {
        return false;
    }
    (0..3).all(|channel| {
        let pixel = u16::from(pixel[channel]);
        let straight = u16::from(background[channel]);
        let premultiplied = straight * alpha / 255;
        pixel.abs_diff(straight).min(pixel.abs_diff(premultiplied)) <= 10
    })
}

fn erase_with_alpha(pixel: &mut Rgba<u8>, alpha: u8) {
    let keep = 255 - u16::from(alpha);
    pixel[3] = (u16::from(pixel[3]) * keep / 255) as u8;
    if pixel[3] == 0 {
        pixel.0[..3].fill(0);
    }
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
    background: vec4<f32>,
    background_reference: vec4<f32>,
    material: array<vec4<f32>, 16>,
    optical: array<vec4<f32>, 16>,
    annotation: array<vec4<f32>, 16>,
    surface_override: array<vec4<f32>, 16>,
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

fn source_background_color(uv: vec2<f32>) -> vec3<f32> {
    let source = textureSample(layers, layer_sampler, uv, 0);
    if params.background.a < 0.5 { return source.rgb; }
    let source_luma = dot(source.rgb, vec3<f32>(0.2126, 0.7152, 0.0722));
    let shade = clamp(source_luma / params.background_reference.x, 0.18, 2.75);
    return clamp(params.background.rgb * shade, vec3<f32>(0.0), vec3<f32>(1.0));
}

fn source_with_surface_override(source: vec4<f32>, setting: vec4<f32>) -> vec4<f32> {
    if setting.x < 0.0 { return vec4<f32>(source.rgb, source.a * setting.w); }
    let source_luma = dot(source.rgb, vec3<f32>(0.2126, 0.7152, 0.0722));
    let target_luma = max(dot(setting.rgb, vec3<f32>(0.2126, 0.7152, 0.0722)), 0.0001);
    let recolored = clamp(setting.rgb * (source_luma / target_luma), vec3<f32>(0.0), vec3<f32>(1.0));
    return vec4<f32>(recolored, source.a * setting.w);
}

fn tilted_uv(uv: vec2<f32>) -> vec2<f32> {
    if params.pointer.w < 0.5 { return uv; }
    let centered = uv - vec2<f32>(0.5);
    let tilt = params.pointer.xy * 0.10;
    let depth = max(0.82, 1.0 + dot(centered, tilt));
    return vec2<f32>(0.5) + (centered - tilt * 0.030) / depth;
}

@fragment
fn fs_main(input: VertexOut) -> @location(0) vec4<f32> {
    let uv = tilted_uv(input.uv);
    let source_background = textureSample(layers, layer_sampler, uv, 0);
    var color = source_background_color(uv);
    let environment = preview_background(uv + params.pointer.xy * 0.008);
    if params.pointer.z >= 0.0 {
        let selected_index = i32(params.pointer.z + 0.5);
        var selected = textureSample(layers, layer_sampler, uv, selected_index);
        if selected_index == 0 {
            return vec4(source_background_color(uv), 1.0);
        }
        selected = source_with_surface_override(selected, params.surface_override[u32(selected_index - 1)]);
        return vec4(mix(environment, selected.rgb, selected.a), 1.0);
    }

    let foreground_count = max(params.state.y - 1.0, 1.0);
    let mode = params.state.x;

    for (var index: i32 = 1; index < 17; index = index + 1) {
        if f32(index) >= params.state.y { break; }
        let surface_index = u32(index - 1);
        let material_settings = params.material[surface_index];
        let optical_settings = params.optical[surface_index];
        let annotation = params.annotation[surface_index];
        let depth_plane = select(f32(index), optical_settings.w, optical_settings.w >= 0.0);
        let z = depth_plane / foreground_count;
        let parallax = params.pointer.xy * z * 0.036;
        let sample_uv = uv + parallax;
        let source = source_with_surface_override(
            textureSample(layers, layer_sampler, sample_uv, index),
            params.surface_override[surface_index],
        );
        var source_alpha = source.a;
        var effects_enabled = material_settings.x > 0.5;
        if mode > 0.5 && mode < 1.5 {
            if annotation.y >= 0.0 { source_alpha = source_alpha * annotation.y; }
            if annotation.z >= 0.0 { effects_enabled = annotation.z > 0.5; }
        } else if mode > 1.5 {
            if annotation.x >= 0.0 { source_alpha = source_alpha * annotation.x; }
            if annotation.w >= 0.0 { effects_enabled = annotation.w > 0.5; }
        }
        if source_alpha <= 0.0001 { continue; }
        if !effects_enabled {
            color = mix(color, source.rgb, source_alpha * params.accent.a);
            continue;
        }
        let shadow_alpha = textureSample(
            layers,
            layer_sampler,
            sample_uv + vec2<f32>(0.0, 0.004 + optical_settings.z * 0.014 + z * 0.008),
            index,
        ).a;
        color = color
            * (1.0 - shadow_alpha * (0.020 + optical_settings.z * 0.100) * params.accent.a);

        let gradient_step = 0.003 + material_settings.z * 0.008;
        let gradient = vec2<f32>(
            textureSample(layers, layer_sampler, sample_uv + vec2<f32>(gradient_step, 0.0), index).a
                - textureSample(layers, layer_sampler, sample_uv - vec2<f32>(gradient_step, 0.0), index).a,
            textureSample(layers, layer_sampler, sample_uv + vec2<f32>(0.0, gradient_step), index).a
                - textureSample(layers, layer_sampler, sample_uv - vec2<f32>(0.0, gradient_step), index).a,
        );
        let edge = clamp(length(gradient), 0.0, 1.0);
        let normal = gradient / max(edge, 0.0001);
        let from_above = max(dot(normal, vec2<f32>(0.0, -1.0)), 0.0);
        let specular_alignment = select(
            from_above,
            1.0 - from_above,
            material_settings.y > 2.5,
        );
        let specular_strength = select(0.0, 0.11 + z * 0.14, material_settings.y > 0.5);
        let specular = vec3<f32>(0.82, 0.90, 1.0)
            * edge
            * specular_strength
            * (0.35 + specular_alignment * 0.65);
        let refraction_vector = optical_settings.xy - vec2<f32>(0.5);
        let refracted_uv = sample_uv
            - normal * (0.002 + z * 0.005 + material_settings.z * 0.004)
            + refraction_vector * 0.018;
        var behind = source_background_color(refracted_uv);
        for (var behind_index: i32 = 1; behind_index < 17; behind_index = behind_index + 1) {
            if behind_index >= index { break; }
            let behind_layer = source_with_surface_override(
                textureSample(layers, layer_sampler, refracted_uv, behind_index),
                params.surface_override[u32(behind_index - 1)],
            );
            behind = mix(behind, behind_layer.rgb, behind_layer.a);
        }
        let blur_offset = vec2<f32>(material_settings.z * 0.006, 0.0);
        let blurred_behind = (
            behind
            + source_background_color(refracted_uv + blur_offset)
            + source_background_color(refracted_uv - blur_offset)
        ) / 3.0;
        let refracted = mix(source.rgb, blurred_behind, 0.04 + material_settings.w * 0.26);
        let material = refracted + specular;
        let layer_alpha = source_alpha
            * (0.60 + material_settings.w * 0.28 + z * 0.10)
            * params.accent.a;
        color = color * (1.0 - layer_alpha) + material * layer_alpha;
    }

    let luminance = dot(color, vec3<f32>(0.2126, 0.7152, 0.0722));
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
    let glass = mix(artwork, refracted, glass_mix);

    if params.state.w > 0.5 {
        var enclosure_alpha = 1.0;
        if mode > 1.5 && mode < 2.5 {
            enclosure_alpha = 0.68;
        } else if mode > 2.5 && mode < 3.5 {
            enclosure_alpha = 0.76;
        } else if mode > 3.5 && mode < 4.5 {
            enclosure_alpha = 0.86;
        } else if mode > 4.5 {
            enclosure_alpha = 0.90;
        }
        return vec4<f32>(glass, enclosure_alpha);
    }
    let canvas = environment;
    return vec4<f32>(mix(canvas, glass, source_background.a), 1.0);
}
"#;

fn glass_shader() -> String {
    GLASS_SHADER_TEMPLATE.to_owned()
}

#[cfg(any())]
mod legacy_safe_zone_tests {
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

    fn inset_rounded_square(id: &str) -> RasterLayer {
        let source = rounded_square(id).image;
        let resized = imageops::resize(&source, 860, 860, imageops::FilterType::Nearest);
        let mut canvas = RgbaImage::new(CANVAS_SIZE, CANVAS_SIZE);
        imageops::overlay(&mut canvas, &resized, 82, 82);
        layer(id, canvas)
    }

    fn symmetric_layered_svg() -> &'static str {
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="1024" height="1024" viewBox="0 0 1024 1024">
<g id="background"><rect width="1024" height="1024" fill="#203050"/></g>
<g id="foreground-1"><circle cx="512" cy="512" r="300" fill="#5865F2"/></g>
<g id="foreground-2"><circle cx="392" cy="430" r="48" fill="#ffffff"/><circle cx="632" cy="430" r="48" fill="#ffffff"/></g>
<g id="foreground-3"><path d="M392,640 Q512,752 632,640" stroke="#ffffff" stroke-width="36" fill="none"/></g>
</svg>"##
    }

    #[test]
    fn background_never_joins_artwork_bounds_and_oversized_artwork_shrinks() {
        let background = solid("background", [0, 0, 0, 255]);
        // The opaque background never joins the measurement. A 940 px
        // foreground is above the 860 px safe-zone target and shrinks as one
        // unit even though it does not touch the source canvas edge.
        let foreground = rect_layer("foreground-1", 140, 40, 900, 980, [255, 255, 255, 255]);
        let fitted = fit_layers_to_icon_frame(vec![background, foreground]);
        let (min_x, min_y, max_x, max_y) = artwork_bounds(&fitted).unwrap();
        assert!((max_x - min_x + 1) < 760);
        assert!((max_y - min_y + 1) as i32 - SAFE_ZONE_TARGET as i32 <= 4);
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
        let corner = layers[0].image.get_pixel(2, 2);
        assert_ne!(
            corner.0,
            [30, 160, 80, 255],
            "old background leaked into corner"
        );
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

        // A large legacy rounded square is also an enclosure when it frames
        // later artwork, which is the shape emitted for Athas-like icons.
        let rounded = extract_enclosure(vec![
            background.clone(),
            inset_rounded_square("foreground-1"),
            rect_layer("foreground-2", 430, 430, 590, 590, [40, 90, 180, 255]),
        ]);
        assert_eq!(rounded.len(), 2);
        assert_eq!(rounded[0].image.get_pixel(512, 60).0, [255, 255, 255, 255]);
        // Corners become part of the colour field; only the final renderer
        // mask decides their transparency.
        assert_eq!(rounded[0].image.get_pixel(2, 2).0, [255, 255, 255, 255]);
        assert_eq!(rounded[1].id, "foreground-2");
    }

    #[test]
    fn canonical_mask_is_centered_feathered_and_shared_with_shader() {
        assert_eq!(mask_value(mask_distance([0.5, 0.5])), 1.0);
        assert_eq!(mask_value(mask_distance([0.0, 0.0])), 0.0);
        assert_eq!(
            mask_value(mask_distance([0.5 - MASK_RADIUS * 0.85, 0.5])),
            1.0
        );
        let mid_edge = mask_value(mask_distance([0.5 + MASK_RADIUS, 0.5]));
        assert_eq!(mid_edge, 1.0, "midpoint must not look inset");
        assert_eq!(
            mask_value(mask_distance([0.5 + MASK_RADIUS * 1.02, 0.5])),
            0.0
        );

        let mut image = RgbaImage::from_pixel(128, 128, image::Rgba([90, 90, 90, 255]));
        apply_canonical_mask(&mut image);
        assert_eq!(image.get_pixel(64, 64)[3], 255);
        assert_eq!(image.get_pixel(0, 0)[3], 0);
        assert!(image.get_pixel(0, 64)[3] > 200);

        let shader = glass_shader();
        assert!(shader.contains("0.5"), "mask radius missing");
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
        renderer.load_svg(symmetric_layered_svg()).unwrap();
        let image = renderer
            .render(256, 256, RenderSettings::default(), RenderTarget::Icon)
            .unwrap();
        let mut difference = 0u64;
        let mut energy = 0u64;
        for y in 0..256 {
            for x in 0..128 {
                let left = image.get_pixel(x, y);
                let right = image.get_pixel(255 - x, y);
                for channel in 0..3 {
                    difference += u64::from(left[channel].abs_diff(right[channel]));
                    energy += u64::from(left[channel]) + u64::from(right[channel]);
                }
            }
        }
        assert!(energy > 0);
        let asymmetry = difference as f64 / energy as f64;
        assert!(asymmetry <= 0.04, "RGB horizontal asymmetry {asymmetry}");
    }

    #[tokio::test]
    async fn every_layer_view_renders_without_crashing() {
        let Ok(mut renderer) = GlassRenderer::new().await else {
            return;
        };
        renderer.load_svg(symmetric_layered_svg()).unwrap();
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

    #[tokio::test]
    async fn clear_and_tinted_icons_export_real_transparency() {
        let Ok(mut renderer) = GlassRenderer::new().await else {
            return;
        };
        renderer.load_svg(symmetric_layered_svg()).unwrap();
        let default = renderer
            .render(128, 128, RenderSettings::default(), RenderTarget::Icon)
            .unwrap();
        let clear = renderer
            .render(
                128,
                128,
                RenderSettings {
                    appearance: Appearance::ClearLight,
                    ..RenderSettings::default()
                },
                RenderTarget::Icon,
            )
            .unwrap();
        let tinted = renderer
            .render(
                128,
                128,
                RenderSettings {
                    appearance: Appearance::TintedLight,
                    ..RenderSettings::default()
                },
                RenderTarget::Icon,
            )
            .unwrap();
        assert_eq!(default.get_pixel(64, 64)[3], 255);
        assert!((140..220).contains(&clear.get_pixel(64, 64)[3]));
        assert!((200..245).contains(&tinted.get_pixel(64, 64)[3]));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LEGACY_SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" width="1024" height="1024" viewBox="0 0 1024 1024">
<g id="background"><rect width="1024" height="1024" fill="#203050"/></g>
<g id="foreground-1"><circle cx="260" cy="512" r="96" fill="#ff2030"/></g>
<g id="foreground-2"><circle cx="764" cy="512" r="96" fill="#20b070"/></g>
</svg>"##;

    const NESTED_SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" width="1024" height="1024" viewBox="0 0 1024 1024">
<g id="background"><rect width="1024" height="1024" fill="#203050"/></g>
<g id="group-1" data-liquid-mode="combined" data-liquid-specular="inside" data-liquid-translucency="0.7">
  <g id="layer-1-1"><circle cx="512" cy="512" r="280" fill="#5865f2"/></g>
  <g id="layer-1-2"><circle cx="420" cy="450" r="42" fill="#fff"/></g>
</g>
<g id="group-2" data-liquid-effects="false">
  <g id="layer-2-1"><circle cx="604" cy="450" r="42" fill="#fff"/></g>
</g>
</svg>"##;

    const BACKGROUND_COLORED_CUTOUT_SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" width="1024" height="1024" viewBox="0 0 1024 1024">
<g id="background"><rect width="1024" height="1024" fill="#5865f2"/></g>
<g id="group-1"><g id="layer-1-1"><rect x="192" y="256" width="640" height="512" rx="180" fill="#ffffff"/></g></g>
<g id="group-2"><g id="layer-2-1"><circle cx="406" cy="529" r="64" fill="#5865f2"/><circle cx="618" cy="529" r="64" fill="#5865f2"/></g></g>
</svg>"##;

    const RGB_SYNTAX_CUTOUT_SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" width="1024" height="1024" viewBox="0 0 1024 1024">
<g id="background"><rect width="1024" height="1024" fill="#5865f2"/></g>
<g id="group-1"><g id="layer-1-1"><rect x="192" y="256" width="640" height="512" rx="180" fill="#ffffff"/></g></g>
<g id="group-2"><g id="layer-2-1"><circle cx="406" cy="529" r="64" fill="rgb(88, 101, 242)"/></g></g>
</svg>"##;

    const GRADIENT_BACKGROUND_SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" width="1024" height="1024" viewBox="0 0 1024 1024">
<defs><linearGradient id="background-gradient" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="#dbeafe"/><stop offset="1" stop-color="#1e3a8a"/></linearGradient></defs>
<g id="background"><rect width="1024" height="1024" fill="url(#background-gradient)"/></g>
<g id="group-1"><g id="layer-1-1"><circle cx="512" cy="512" r="140" fill="#ffffff"/></g></g>
</svg>"##;

    #[test]
    fn canonical_layers_keep_the_source_grid() {
        let layers = prepare_canonical_layers(LEGACY_SVG).unwrap();
        assert_eq!(layers.len(), 3);
        assert!(layers[1].image.get_pixel(260, 512)[3] > 0);
        assert_eq!(layers[1].image.get_pixel(512, 512)[3], 0);
        assert!(layers[2].image.get_pixel(764, 512)[3] > 0);
    }

    #[test]
    fn combined_groups_resolve_to_one_material_surface() {
        let document = prepare_canonical_document(NESTED_SVG).unwrap();
        assert_eq!(document.surfaces.len(), 2);
        assert_eq!(document.surfaces[0].label, "Group 1 (Combined)");
        assert!(document.surfaces[0].settings.material.effects_enabled);
        assert!(!document.surfaces[1].settings.material.effects_enabled);
        assert!(document.surfaces[0].image.get_pixel(420, 450)[3] > 0);
    }

    #[test]
    fn background_colored_foreground_details_become_real_cutouts() {
        let document = prepare_canonical_document(BACKGROUND_COLORED_CUTOUT_SVG).unwrap();
        assert_eq!(document.surfaces.len(), 2);
        assert_eq!(
            document.background.get_pixel(406, 529).0,
            [88, 101, 242, 255]
        );
        assert_eq!(document.surfaces[0].image.get_pixel(406, 529)[3], 0);
        assert_eq!(document.surfaces[1].image.get_pixel(406, 529)[3], 0);
        assert_eq!(document.surfaces[0].image.get_pixel(512, 340)[3], 255);
    }

    #[test]
    fn rgb_background_colored_details_are_cut_out_too() {
        let document = prepare_canonical_document(RGB_SYNTAX_CUTOUT_SVG).unwrap();
        assert_eq!(document.surfaces[0].image.get_pixel(406, 529)[3], 0);
        assert_eq!(document.surfaces[1].image.get_pixel(406, 529)[3], 0);
    }

    #[test]
    fn rasterized_layers_use_straight_alpha() {
        let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="1024" height="1024" viewBox="0 0 1024 1024">
<g id="background"><rect width="1024" height="1024" fill="#203050"/></g>
<g id="group-1"><g id="layer-1-1"><rect x="100" y="100" width="200" height="200" fill="#ffffff" opacity="0.5"/></g></g>
</svg>"##;
        let document = rasterize_document(svg).unwrap();
        let pixel = document.groups[0].layers[0].image.get_pixel(150, 150);
        assert!((120..=135).contains(&pixel[3]));
        assert!(pixel[0] > 245 && pixel[1] > 245 && pixel[2] > 245);
    }

    #[test]
    fn foreground_opacity_is_a_clamped_material_uniform() {
        let faint = RenderSettings {
            foreground_opacity: -2.0,
            ..RenderSettings::default()
        }
        .params(RenderTarget::Icon, &[], 1.0);
        let vivid = RenderSettings {
            foreground_opacity: 3.0,
            ..RenderSettings::default()
        }
        .params(RenderTarget::Icon, &[], 1.0);
        assert_eq!(faint[3], 0.20);
        assert_eq!(vivid[3], 1.50);
    }

    #[test]
    fn surface_uniforms_are_grouped_for_wgsl_arrays() {
        let first = SurfaceSettings {
            material: MaterialSettings {
                blur: 0.2,
                ..MaterialSettings::default()
            },
            dark: AppearanceAnnotation::default(),
            mono: AppearanceAnnotation::default(),
        };
        let second = SurfaceSettings {
            material: MaterialSettings {
                blur: 0.8,
                ..MaterialSettings::default()
            },
            dark: AppearanceAnnotation::default(),
            mono: AppearanceAnnotation::default(),
        };
        let settings = RenderSettings {
            surface_overrides: [
                SurfaceOverride {
                    color: Some([255, 0, 0]),
                    opacity: 0.4,
                    plane: Some(1),
                },
                SurfaceOverride {
                    color: Some([0, 255, 0]),
                    opacity: 0.7,
                    plane: None,
                },
                SurfaceOverride::default(),
                SurfaceOverride::default(),
                SurfaceOverride::default(),
                SurfaceOverride::default(),
                SurfaceOverride::default(),
                SurfaceOverride::default(),
                SurfaceOverride::default(),
                SurfaceOverride::default(),
                SurfaceOverride::default(),
                SurfaceOverride::default(),
                SurfaceOverride::default(),
                SurfaceOverride::default(),
                SurfaceOverride::default(),
                SurfaceOverride::default(),
            ],
            ..RenderSettings::default()
        }
        .params(RenderTarget::Icon, &[first, second], 0.5);
        const BASE: usize = 20;
        assert_eq!(settings[BASE + 2], 0.2);
        assert_eq!(settings[BASE + 4 + 2], 0.8);
        let optical = BASE + MAX_SURFACES * 4;
        assert_eq!(settings[optical + 3], 1.0);
        assert_eq!(settings[optical + 7], -1.0);
        let overrides = BASE + MAX_SURFACES * 12;
        assert_eq!(settings[overrides + 3], 0.4);
        assert_eq!(settings[overrides + 7], 0.7);
    }

    #[test]
    fn canonical_mask_is_centered_and_applied_once() {
        let mut image = RgbaImage::from_pixel(128, 128, image::Rgba([90, 90, 90, 255]));
        apply_canonical_mask(&mut image);
        assert_eq!(image.get_pixel(64, 64)[3], 255);
        assert_eq!(image.get_pixel(0, 0)[3], 0);
        assert!(image.get_pixel(0, 64)[3] > 200);
    }

    #[tokio::test]
    async fn group_and_layer_inspection_render_without_crashing() {
        let Ok(mut renderer) = GlassRenderer::new().await else {
            return;
        };
        renderer.load_svg(NESTED_SVG).unwrap();
        assert_eq!(
            renderer.inspect_labels(),
            ["Background", "Group 1 (Combined)", "Group 2 / Layer 1"]
        );
        for layer in 0..renderer.layer_count() {
            let image = renderer
                .render(
                    128,
                    128,
                    RenderSettings {
                        layer: Some(layer),
                        ..RenderSettings::default()
                    },
                    RenderTarget::Preview,
                )
                .unwrap();
            assert_eq!(image.dimensions(), (128, 128));
        }
        let icon = renderer
            .render(128, 128, RenderSettings::default(), RenderTarget::Icon)
            .unwrap();
        assert_eq!(icon.get_pixel(0, 0)[3], 0);
        assert!(icon.get_pixel(64, 64)[3] > 0);
    }

    #[tokio::test]
    async fn background_override_changes_only_the_runtime_render() {
        let Ok(mut renderer) = GlassRenderer::new().await else {
            return;
        };
        renderer.load_svg(NESTED_SVG).unwrap();
        let source = renderer
            .render(128, 128, RenderSettings::default(), RenderTarget::Icon)
            .unwrap();
        let overridden = renderer
            .render(
                128,
                128,
                RenderSettings {
                    background: Some([232, 48, 70]),
                    ..RenderSettings::default()
                },
                RenderTarget::Icon,
            )
            .unwrap();
        let source_pixel = source.get_pixel(64, 20);
        let override_pixel = overridden.get_pixel(64, 20);
        assert!(override_pixel[0] > override_pixel[1] + 40);
        assert_ne!(source_pixel, override_pixel);
    }

    #[tokio::test]
    async fn background_override_preserves_source_gradient_contrast() {
        let Ok(mut renderer) = GlassRenderer::new().await else {
            return;
        };
        renderer.load_svg(GRADIENT_BACKGROUND_SVG).unwrap();
        let image = renderer
            .render(
                128,
                128,
                RenderSettings {
                    background: Some([220, 48, 48]),
                    ..RenderSettings::default()
                },
                RenderTarget::Icon,
            )
            .unwrap();
        let top = image.get_pixel(64, 20);
        let bottom = image.get_pixel(64, 108);
        assert!(top[0] > top[1] + 25 && bottom[0] > bottom[1] + 10);
        assert!(top[0] > bottom[0] + 20, "gradient was flattened");
    }

    #[tokio::test]
    async fn surface_override_recolors_only_the_selected_foreground() {
        let Ok(mut renderer) = GlassRenderer::new().await else {
            return;
        };
        renderer.load_svg(GRADIENT_BACKGROUND_SVG).unwrap();
        let image = renderer
            .render(
                128,
                128,
                RenderSettings {
                    surface_overrides: [
                        SurfaceOverride {
                            color: Some([230, 35, 35]),
                            opacity: 1.0,
                            plane: None,
                        },
                        SurfaceOverride::default(),
                        SurfaceOverride::default(),
                        SurfaceOverride::default(),
                        SurfaceOverride::default(),
                        SurfaceOverride::default(),
                        SurfaceOverride::default(),
                        SurfaceOverride::default(),
                        SurfaceOverride::default(),
                        SurfaceOverride::default(),
                        SurfaceOverride::default(),
                        SurfaceOverride::default(),
                        SurfaceOverride::default(),
                        SurfaceOverride::default(),
                        SurfaceOverride::default(),
                        SurfaceOverride::default(),
                    ],
                    ..RenderSettings::default()
                },
                RenderTarget::Icon,
            )
            .unwrap();
        let foreground = image.get_pixel(64, 64);
        let background = image.get_pixel(64, 20);
        assert!(foreground[0] > foreground[1] + 25);
        assert_ne!(foreground, background);
    }

    #[tokio::test]
    async fn cutouts_follow_the_runtime_background_and_foreground_opacity() {
        let Ok(mut renderer) = GlassRenderer::new().await else {
            return;
        };
        renderer.load_svg(BACKGROUND_COLORED_CUTOUT_SVG).unwrap();
        let faint = renderer
            .render(
                128,
                128,
                RenderSettings {
                    foreground_opacity: 0.20,
                    ..RenderSettings::default()
                },
                RenderTarget::Icon,
            )
            .unwrap();
        let vivid = renderer
            .render(
                128,
                128,
                RenderSettings {
                    foreground_opacity: 1.50,
                    ..RenderSettings::default()
                },
                RenderTarget::Icon,
            )
            .unwrap();
        assert_eq!(faint.get_pixel(64, 16), vivid.get_pixel(64, 16));
        assert_ne!(faint.get_pixel(64, 64), vivid.get_pixel(64, 64));

        let overridden = renderer
            .render(
                128,
                128,
                RenderSettings {
                    background: Some([232, 48, 70]),
                    ..RenderSettings::default()
                },
                RenderTarget::Icon,
            )
            .unwrap();
        let eye = overridden.get_pixel(51, 66);
        let background = overridden.get_pixel(16, 64);
        assert_eq!(eye, background);
        assert!(eye[0] > eye[1] + 50 && eye[0] > eye[2] + 50);
    }

    #[tokio::test]
    async fn pointer_tilt_changes_preview_but_not_exported_icon() {
        let Ok(mut renderer) = GlassRenderer::new().await else {
            return;
        };
        renderer.load_svg(NESTED_SVG).unwrap();
        let static_icon = renderer
            .render(128, 128, RenderSettings::default(), RenderTarget::Icon)
            .unwrap();
        let moving_icon = renderer
            .render(
                128,
                128,
                RenderSettings {
                    pointer: [0.8, -0.6],
                    tilt: true,
                    ..RenderSettings::default()
                },
                RenderTarget::Icon,
            )
            .unwrap();
        assert_eq!(static_icon, moving_icon);

        let preview = renderer
            .render(128, 128, RenderSettings::default(), RenderTarget::Preview)
            .unwrap();
        let tilted_preview = renderer
            .render(
                128,
                128,
                RenderSettings {
                    pointer: [0.8, -0.6],
                    tilt: true,
                    ..RenderSettings::default()
                },
                RenderTarget::Preview,
            )
            .unwrap();
        assert_ne!(preview, tilted_preview);
    }
}
