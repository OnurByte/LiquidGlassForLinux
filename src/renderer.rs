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
const ICON_ARTWORK_SIZE: u32 = 760;

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
}

impl Default for RenderSettings {
    fn default() -> Self {
        Self {
            appearance: Appearance::Default,
            accent: [137, 180, 250],
            dark_background: false,
            pointer: [0.0, 0.0],
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
        let layers = fit_layers_to_icon_frame(rasterize_layers(svg)?);
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
        RgbaImage::from_raw(width, height, pixels)
            .ok_or_else(|| gpu_error("GPU returned an invalid RGBA image"))
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
        source: wgpu::ShaderSource::Wgsl(GLASS_SHADER.into()),
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
            0.0,
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

fn fit_layers_to_icon_frame(mut layers: Vec<RasterLayer>) -> Vec<RasterLayer> {
    let Some((min_x, min_y, max_x, max_y)) = foreground_bounds(&layers) else {
        return layers;
    };
    let source_width = max_x - min_x + 1;
    let source_height = max_y - min_y + 1;
    let scale = ICON_ARTWORK_SIZE as f32 / source_width.max(source_height) as f32;
    let target_width = ((source_width as f32 * scale).round() as u32).max(1);
    let target_height = ((source_height as f32 * scale).round() as u32).max(1);
    let target_x = (CANVAS_SIZE - target_width) / 2;
    let target_y = (CANVAS_SIZE - target_height) / 2;
    for layer in layers.iter_mut().skip(1) {
        let source =
            imageops::crop_imm(&layer.image, min_x, min_y, source_width, source_height).to_image();
        let resized = imageops::resize(
            &source,
            target_width,
            target_height,
            imageops::FilterType::Lanczos3,
        );
        let mut framed = RgbaImage::new(CANVAS_SIZE, CANVAS_SIZE);
        imageops::overlay(
            &mut framed,
            &resized,
            i64::from(target_x),
            i64::from(target_y),
        );
        layer.image = framed;
    }
    layers
}

fn foreground_bounds(layers: &[RasterLayer]) -> Option<(u32, u32, u32, u32)> {
    let mut min_x = CANVAS_SIZE;
    let mut min_y = CANVAS_SIZE;
    let mut max_x = 0;
    let mut max_y = 0;
    let mut found = false;
    for layer in layers.iter().skip(1) {
        for (x, y, pixel) in layer.image.enumerate_pixels() {
            if pixel[3] <= 8 {
                continue;
            }
            found = true;
            min_x = min_x.min(x);
            min_y = min_y.min(y);
            max_x = max_x.max(x);
            max_y = max_y.max(y);
        }
    }
    found.then_some((min_x, min_y, max_x, max_y))
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

const GLASS_SHADER: &str = r#"
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

fn icon_mask(uv: vec2<f32>) -> f32 {
    let p = abs((uv - vec2<f32>(0.5)) / vec2<f32>(0.41));
    let distance = pow(p.x, 4.2) + pow(p.y, 4.2);
    return 1.0 - smoothstep(0.94, 1.0, distance);
}

@fragment
fn fs_main(input: VertexOut) -> @location(0) vec4<f32> {
    let uv = input.uv;
    let mask = icon_mask(uv);
    let source_background = textureSample(layers, layer_sampler, uv, 0);
    var color = source_background.rgb;
    var alpha = source_background.a;
    let foreground_count = max(params.state.y - 1.0, 1.0);

    for (var index: i32 = 1; index < 5; index = index + 1) {
        if f32(index) >= params.state.y { break; }
        let z = f32(index) / foreground_count;
        let parallax = params.pointer.xy * z * 0.036;
        let depth_gap = vec2<f32>(0.0025, -0.0018) * z;
        let sample_uv = uv + parallax + depth_gap;
        let source = textureSample(layers, layer_sampler, sample_uv, index);
        let shadow = textureSample(
            layers,
            layer_sampler,
            sample_uv + vec2<f32>(0.015, 0.022) * (0.55 + z),
            index,
        );
        color = color * (1.0 - shadow.a * (0.18 + z * 0.16));

        let edge_x = textureSample(layers, layer_sampler, sample_uv + vec2<f32>(0.006, 0.0), index).a;
        let edge_y = textureSample(layers, layer_sampler, sample_uv + vec2<f32>(0.0, 0.006), index).a;
        let edge = clamp(abs(source.a - edge_x) + abs(source.a - edge_y), 0.0, 1.0);
        let specular = vec3<f32>(0.78, 0.86, 1.0) * edge * (0.16 + z * 0.16);
        let refracted = mix(source.rgb, color, 0.08 + z * 0.10);
        let material = refracted + specular;
        color = color * (1.0 - source.a) + material * source.a;
        alpha = alpha + source.a * (1.0 - alpha);
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
        glass_mix = select(0.34, 0.46, mode > 4.5);
    }

    let edge = smoothstep(
        0.58,
        0.98,
        pow(abs((uv.x - 0.5) / 0.41), 4.2) + pow(abs((uv.y - 0.5) / 0.41), 4.2),
    );
    let environment = preview_background(uv + params.pointer.xy * 0.008);
    let refracted = select(color, environment, params.state.w < 0.5);
    var glass = mix(artwork, refracted, glass_mix);
    let highlight = pow(max(0.0, 1.0 - distance(uv, vec2<f32>(0.34, 0.28)) * 1.8), 4.0) * 0.28;
    glass = glass + vec3<f32>(highlight + edge * 0.10);

    if params.state.w > 0.5 {
        return vec4<f32>(glass, mask * alpha);
    }
    let canvas = environment;
    return vec4<f32>(mix(canvas, glass, mask * alpha), 1.0);
}
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_non_square_foreground_inside_the_icon_frame() {
        let mut background = RgbaImage::new(CANVAS_SIZE, CANVAS_SIZE);
        for pixel in background.pixels_mut() {
            *pixel = image::Rgba([0, 0, 0, 255]);
        }
        let mut foreground = RgbaImage::new(CANVAS_SIZE, CANVAS_SIZE);
        for y in 40..980 {
            for x in 140..900 {
                foreground.put_pixel(x, y, image::Rgba([255, 255, 255, 255]));
            }
        }
        let layers = fit_layers_to_icon_frame(vec![
            RasterLayer {
                id: "background".to_owned(),
                image: background,
            },
            RasterLayer {
                id: "foreground-1".to_owned(),
                image: foreground,
            },
        ]);
        let bounds = foreground_bounds(&layers).unwrap();
        assert!(bounds.2 - bounds.0 < ICON_ARTWORK_SIZE + 2);
        assert!(bounds.3 - bounds.1 < ICON_ARTWORK_SIZE + 2);
        assert_eq!(layers[0].image.get_pixel(0, 0)[3], 255);
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
<g id="foreground-1"><circle cx="512" cy="512" r="240" fill="#ffffff"/></g>
</svg>"##,
            )
            .unwrap();
        let image = renderer
            .render(128, 128, RenderSettings::default(), RenderTarget::Icon)
            .unwrap();
        assert!(image.get_pixel(64, 64)[3] > 0);
    }
}
