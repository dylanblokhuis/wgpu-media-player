use std::{num::NonZeroU32, sync::Arc};

use wgpu::util::DeviceExt;
use winit::dpi::PhysicalSize;

use crate::texture::Texture;

pub const INDICES: &[u16] = &[0, 1, 2, 3, 4, 5];

pub struct VideoRenderer {
    window_size: PhysicalSize<u32>,
    video_size: PhysicalSize<u32>,
    pub render_pipeline: wgpu::RenderPipeline,
    pub bind_group: wgpu::BindGroup,
    pub vertex_buffer: wgpu::Buffer,
    pub index_buffer: wgpu::Buffer,
    texture: Texture,
}

impl VideoRenderer {
    pub fn new(
        window_size: PhysicalSize<u32>,
        video_size: PhysicalSize<u32>,
        device: Arc<wgpu::Device>,
        config: wgpu::SurfaceConfiguration,
    ) -> Self {
        let texture_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                entries: &[
                    wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            multisampled: false,
                            view_dimension: wgpu::TextureViewDimension::D2,
                            sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 1,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        // This should match the filterable field of the
                        // corresponding Texture entry above.
                        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                        count: None,
                    },
                ],
                label: Some("texture_bind_group_layout"),
            });

        let render_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("Render Pipeline Layout"),
                bind_group_layouts: &[&texture_bind_group_layout],
                push_constant_ranges: &[],
            });

        let texture_to_render = Texture::new(
            &device,
            (video_size.width, video_size.height),
            Some("Video"),
        )
        .unwrap();

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            layout: &texture_bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&texture_to_render.view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&texture_to_render.sampler),
                },
            ],
            label: Some("diffuse_bind_group"),
        });

        let vertex_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("Vertex Buffer"),
            contents: bytemuck::cast_slice(&VideoRenderer::get_vertices(window_size, video_size)),
            usage: wgpu::BufferUsages::VERTEX,
        });
        let index_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("Index Buffer"),
            contents: bytemuck::cast_slice(INDICES),
            usage: wgpu::BufferUsages::INDEX,
        });

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shader.wgsl").into()),
        });

        let render_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("Render Pipeline"),
            layout: Some(&render_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: "vs_main",
                buffers: &[Vertex::desc()],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: "fs_main",
                targets: &[Some(wgpu::ColorTargetState {
                    format: config.format,
                    blend: Some(wgpu::BlendState {
                        color: wgpu::BlendComponent::REPLACE,
                        alpha: wgpu::BlendComponent::REPLACE,
                    }),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                strip_index_format: None,
                front_face: wgpu::FrontFace::Ccw,
                cull_mode: Some(wgpu::Face::Back),
                // Setting this to anything other than Fill requires Features::POLYGON_MODE_LINE
                // or Features::POLYGON_MODE_POINT
                polygon_mode: wgpu::PolygonMode::Fill,
                // Requires Features::DEPTH_CLIP_CONTROL
                unclipped_depth: false,
                // Requires Features::CONSERVATIVE_RASTERIZATION
                conservative: false,
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState {
                count: 1,
                mask: !0,
                alpha_to_coverage_enabled: false,
            },
            // If the pipeline will be used with a multiview render pass, this
            // indicates how many array layers the attachments will have.
            multiview: None,
        });

        Self {
            window_size,
            video_size,
            bind_group,
            index_buffer,
            render_pipeline,
            vertex_buffer,
            texture: texture_to_render,
        }
    }

    pub fn new_frame(&self, queue: &wgpu::Queue, data: &[u8]) {
        queue.write_texture(
            wgpu::ImageCopyTexture {
                aspect: wgpu::TextureAspect::All,
                texture: &self.texture.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
            },
            data,
            wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: NonZeroU32::new(4 * self.video_size.width),
                rows_per_image: NonZeroU32::new(self.video_size.height),
            },
            wgpu::Extent3d {
                width: self.video_size.width,
                height: self.video_size.height,
                depth_or_array_layers: 1,
            },
        );
    }

    // resize vertex buffer, black bars etc..
    pub fn handle_resize(&mut self, device: &wgpu::Device, size: PhysicalSize<u32>) {
        self.window_size = size;
        self.vertex_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("Vertex Buffer"),
            contents: bytemuck::cast_slice(&VideoRenderer::get_vertices(size, self.video_size)),
            usage: wgpu::BufferUsages::VERTEX,
        });
    }

    fn get_vertices(window_size: PhysicalSize<u32>, video_size: PhysicalSize<u32>) -> Vec<Vertex> {
        let screen_width = window_size.width as f32;
        let screen_height = window_size.height as f32;

        let desired_aspect_ratio = video_size.width as f32 / video_size.height as f32;

        let mut vertex_width = 1.0;
        let mut vertex_height = screen_width / desired_aspect_ratio / screen_height;
        if vertex_height > 1.0 {
            vertex_width = screen_height * desired_aspect_ratio / screen_width;
            vertex_height = 1.0;
        }

        let top_left: [f32; 3] = [-vertex_width, vertex_height, 0.0];
        let bottom_left: [f32; 3] = [-vertex_width, -vertex_height, 0.0];
        let top_right: [f32; 3] = [vertex_width, vertex_height, 0.0];
        let bottom_right: [f32; 3] = [vertex_width, -vertex_height, 0.0];

        vec![
            Vertex {
                position: top_left,
                tex_coords: [0.0, 0.0],
            },
            Vertex {
                position: bottom_left,
                tex_coords: [0.0, 1.0],
            },
            Vertex {
                position: bottom_right,
                tex_coords: [1.0, 1.0],
            },
            // second triangle
            Vertex {
                position: top_left,
                tex_coords: [0.0, 0.0],
            },
            Vertex {
                position: bottom_right,
                tex_coords: [1.0, 1.0],
            },
            Vertex {
                position: top_right,
                tex_coords: [1.0, 0.0],
            },
        ]
    }
}

#[repr(C)]
#[derive(Copy, Clone, Debug, bytemuck::Pod, bytemuck::Zeroable)]
struct Vertex {
    position: [f32; 3],
    tex_coords: [f32; 2], // NEW!
}

impl Vertex {
    fn desc<'a>() -> wgpu::VertexBufferLayout<'a> {
        use std::mem;
        wgpu::VertexBufferLayout {
            array_stride: mem::size_of::<Vertex>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &[
                wgpu::VertexAttribute {
                    offset: 0,
                    shader_location: 0,
                    format: wgpu::VertexFormat::Float32x3,
                },
                wgpu::VertexAttribute {
                    offset: mem::size_of::<[f32; 3]>() as wgpu::BufferAddress,
                    shader_location: 1,
                    format: wgpu::VertexFormat::Float32x2, // NEW!
                },
            ],
        }
    }
}
