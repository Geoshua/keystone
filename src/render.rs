//! wgpu side: uploads the latest source frame and draws it through the keystone
//! shader inside egui's render pass (via an egui_wgpu paint callback).

use std::num::NonZeroU64;

use eframe::egui_wgpu::{self, wgpu};

use crate::source::SharedFrame;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct Uniforms {
    pub m: [[f32; 4]; 3],
    /// x: encode sRGB manually, y/z: one output pixel in uv, w: grid overlay.
    pub params: [f32; 4],
}

struct SourceTexture {
    texture: wgpu::Texture,
    bind_group: wgpu::BindGroup,
    size: (u32, u32),
}

pub struct WarpResources {
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    uniforms: wgpu::Buffer,
    source: Option<SourceTexture>,
    uploaded_seq: u64,
    encode_srgb: bool,
}

impl WarpResources {
    pub fn install(rs: &egui_wgpu::RenderState) {
        let device = &rs.device;
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("keystone"),
            source: wgpu::ShaderSource::Wgsl(include_str!("warp.wgsl").into()),
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("keystone"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: NonZeroU64::new(std::mem::size_of::<Uniforms>() as u64),
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("keystone"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("keystone"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(rs.target_format.into())],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("keystone"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let uniforms = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("keystone"),
            size: std::mem::size_of::<Uniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let res = Self {
            pipeline,
            layout,
            sampler,
            uniforms,
            source: None,
            uploaded_seq: 0,
            // The source texture is sRGB, so sampling returns linear values. If the
            // swapchain is not sRGB we have to encode back ourselves.
            encode_srgb: !rs.target_format.is_srgb(),
        };
        rs.renderer.write().callback_resources.insert(res);
    }

    fn upload(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, frame: &SharedFrame) {
        let f = frame.lock().unwrap();
        if f.seq == self.uploaded_seq || f.width == 0 || f.height == 0 {
            return;
        }
        let needed = f.row_pitch as usize * (f.height as usize - 1) + f.width as usize * 4;
        if f.bgra.len() < needed {
            return;
        }
        let size = (f.width, f.height);
        if self.source.as_ref().map(|s| s.size) != Some(size) {
            let texture = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("keystone source"),
                size: wgpu::Extent3d { width: size.0, height: size.1, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Bgra8UnormSrgb,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
            let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("keystone"),
                layout: &self.layout,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: self.uniforms.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&view) },
                    wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(&self.sampler) },
                ],
            });
            self.source = Some(SourceTexture { texture, bind_group, size });
        }
        let tex = &self.source.as_ref().unwrap().texture;
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: tex,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &f.bgra[..needed],
            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(f.row_pitch), rows_per_image: Some(f.height) },
            wgpu::Extent3d { width: f.width, height: f.height, depth_or_array_layers: 1 },
        );
        self.uploaded_seq = f.seq;
    }
}

/// Per-frame paint callback: carries this frame's uniforms and a handle to the frame.
pub struct WarpCallback {
    pub uniforms: Uniforms,
    pub frame: SharedFrame,
}

impl egui_wgpu::CallbackTrait for WarpCallback {
    fn prepare(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        _screen: &egui_wgpu::ScreenDescriptor,
        _encoder: &mut wgpu::CommandEncoder,
        resources: &mut egui_wgpu::CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        let Some(res) = resources.get_mut::<WarpResources>() else { return Vec::new() };
        res.upload(device, queue, &self.frame);
        let mut u = self.uniforms;
        u.params[0] = if res.encode_srgb { 1.0 } else { 0.0 };
        queue.write_buffer(&res.uniforms, 0, bytemuck::bytes_of(&u));
        Vec::new()
    }

    fn paint(&self, _info: eframe::egui::PaintCallbackInfo, pass: &mut wgpu::RenderPass<'static>, resources: &egui_wgpu::CallbackResources) {
        let Some(res) = resources.get::<WarpResources>() else { return };
        let Some(src) = &res.source else { return };
        pass.set_pipeline(&res.pipeline);
        pass.set_bind_group(0, &src.bind_group, &[]);
        pass.draw(0..3, 0..1);
    }
}

/// Source size in pixels, if a frame has arrived.
pub fn frame_size(frame: &SharedFrame) -> Option<(u32, u32)> {
    let f = frame.lock().unwrap();
    (f.width > 0 && f.height > 0).then_some((f.width, f.height))
}
