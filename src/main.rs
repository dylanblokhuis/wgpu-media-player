use crossbeam_channel::Sender;
use rodio::{buffer::SamplesBuffer, OutputStream};
use stainless_ffmpeg::prelude::FormatContext;
use stainless_ffmpeg::prelude::*;
use std::{
    collections::{HashMap, VecDeque},
    num::NonZeroU32,
    slice,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
    u8,
};
use wgpu::util::DeviceExt;
use winit::{
    event::{Event, WindowEvent},
    event_loop::{ControlFlow, EventLoop, EventLoopBuilder},
    window::Window,
};

// mod resampler;
mod texture;

fn main() {
    let event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
    let window = winit::window::Window::new(&event_loop).unwrap();
    window.set_inner_size(winit::dpi::LogicalSize::new(1280, 720));

    pollster::block_on(run(event_loop, window));
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

/**
 * Two triangles that fill the whole screen and position the textures appropriately.
 */
const VERTICES: &[Vertex] = &[
    Vertex {
        position: [-1.0, 1.0, 0.0],
        tex_coords: [0.0, 0.0],
    },
    Vertex {
        position: [-1.0, -1.0, 0.0],
        tex_coords: [0.0, 1.0],
    },
    Vertex {
        position: [1.0, -1.0, 0.0],
        tex_coords: [1.0, 1.0],
    },
    // second triangle
    Vertex {
        position: [-1.0, 1.0, 0.0],
        tex_coords: [0.0, 0.0],
    },
    Vertex {
        position: [1.0, -1.0, 0.0],
        tex_coords: [1.0, 1.0],
    },
    Vertex {
        position: [1.0, 1.0, 0.0],
        tex_coords: [1.0, 0.0],
    },
];

const INDICES: &[u16] = &[0, 1, 2, 3, 4, 5];

#[derive(Debug)]
enum UserEvent {
    NewFrameReady(Vec<u8>),
}

async fn run(event_loop: EventLoop<UserEvent>, window: Window) {
    let size = window.inner_size();

    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: wgpu::Backends::all(),
        ..Default::default()
    });

    let surface = unsafe { instance.create_surface(&window) }.unwrap();
    let adapter = instance
        .request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            force_fallback_adapter: false,
            // Request an adapter which can render to our surface
            compatible_surface: Some(&surface),
        })
        .await
        .expect("Failed to find an appropriate adapter");

    // Create the logical device and command queue
    let (device, queue) = adapter
        .request_device(
            &wgpu::DeviceDescriptor {
                label: None,
                features: wgpu::Features::empty(),
                // Make sure we use the texture resolution limits from the adapter, so we can support images the size of the swapchain.
                limits: wgpu::Limits::downlevel_webgl2_defaults()
                    .using_resolution(adapter.limits()),
            },
            None,
        )
        .await
        .expect("Failed to create device");

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

    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: None,
        bind_group_layouts: &[&texture_bind_group_layout],
        push_constant_ranges: &[],
    });

    let swapchain_capabilities = surface.get_capabilities(&adapter);
    let swapchain_format = swapchain_capabilities.formats[0];

    let mut config = wgpu::SurfaceConfiguration {
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        format: swapchain_format,
        width: size.width,
        height: size.height,
        present_mode: wgpu::PresentMode::Fifo,
        alpha_mode: swapchain_capabilities.alpha_modes[0],
        view_formats: vec![],
    };

    surface.configure(&device, &config);

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
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
            label: Some("texture_bind_group_layout"),
        });

    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("Shader"),
        source: wgpu::ShaderSource::Wgsl(include_str!("shader.wgsl").into()),
    });

    let render_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("Render Pipeline Layout"),
        bind_group_layouts: &[&texture_bind_group_layout],
        push_constant_ranges: &[],
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

    let vertex_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("Vertex Buffer"),
        contents: bytemuck::cast_slice(VERTICES),
        usage: wgpu::BufferUsages::VERTEX,
    });
    let index_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("Index Buffer"),
        contents: bytemuck::cast_slice(INDICES),
        usage: wgpu::BufferUsages::INDEX,
    });
    let num_indices = INDICES.len() as u32;

    // channel
    let (video_sender, video_receiver) = crossbeam_channel::bounded::<Vec<u8>>(1);

    std::thread::spawn(move || {
        decode_video_and_play_audio(video_sender);
    });

    let event_proxy = event_loop.create_proxy();
    std::thread::spawn(move || loop {
        event_proxy
            .send_event(UserEvent::NewFrameReady(
                video_receiver.clone().recv().unwrap(),
            ))
            .unwrap();
    });

    let texture_to_render = texture::Texture::new(&device, (1920, 1080), Some("Video")).unwrap();

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

    event_loop.run(move |event, _, control_flow| {
        // Have the closure take ownership of the resources.
        // `event_loop.run` never returns, therefore we must do this to ensure
        // the resources are properly cleaned up.
        let _ = (&instance, &adapter, &shader, &pipeline_layout);

        *control_flow = ControlFlow::Wait;

        match event {
            Event::WindowEvent {
                event: WindowEvent::Resized(size),
                ..
            } => {
                // Reconfigure the surface with the new size
                config.width = size.width;
                config.height = size.height;
                surface.configure(&device, &config);
                // On macos the window needs to be redrawn manually after resizing
                window.request_redraw();
            }
            Event::RedrawRequested(_) => {
                let frame = surface
                    .get_current_texture()
                    .expect("Failed to acquire next swap chain texture");
                let view = frame
                    .texture
                    .create_view(&wgpu::TextureViewDescriptor::default());
                let mut encoder =
                    device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });

                {
                    let mut render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: None,
                        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                            view: &view,
                            resolve_target: None,
                            ops: wgpu::Operations {
                                load: wgpu::LoadOp::Clear(wgpu::Color::WHITE),
                                store: true,
                            },
                        })],
                        depth_stencil_attachment: None,
                    });
                    render_pass.set_pipeline(&render_pipeline);
                    render_pass.set_bind_group(0, &bind_group, &[]);
                    render_pass.set_vertex_buffer(0, vertex_buffer.slice(..));
                    render_pass.set_index_buffer(index_buffer.slice(..), wgpu::IndexFormat::Uint16); // 1.
                    render_pass.draw_indexed(0..num_indices, 0, 0..1); // 2.
                }

                queue.submit(Some(encoder.finish()));
                frame.present();
            }
            Event::UserEvent(UserEvent::NewFrameReady(data)) => {
                queue.write_texture(
                    wgpu::ImageCopyTexture {
                        aspect: wgpu::TextureAspect::All,
                        texture: &texture_to_render.texture,
                        mip_level: 0,
                        origin: wgpu::Origin3d::ZERO,
                    },
                    &data,
                    wgpu::ImageDataLayout {
                        offset: 0,
                        bytes_per_row: NonZeroU32::new(4 * 1920),
                        rows_per_image: NonZeroU32::new(1080),
                    },
                    wgpu::Extent3d {
                        width: 1920,
                        height: 1080,
                        depth_or_array_layers: 1,
                    },
                );
                window.request_redraw();
            }
            Event::WindowEvent {
                event: WindowEvent::CloseRequested,
                ..
            } => *control_flow = ControlFlow::Exit,
            _ => {}
        }
    });
}

fn decode_video_and_play_audio(video_sender: Sender<Vec<u8>>) {
    let path = std::env::args().nth(1).expect("No file provided");
    let mut format_context = FormatContext::new(&path).unwrap();
    format_context.open_input().unwrap();

    let mut first_audio_stream = None;
    let mut first_video_stream = None;
    for i in 0..format_context.get_nb_streams() {
        let stream_type = format_context.get_stream_type(i as isize);
        println!("Stream {}: {:?}", i, stream_type);

        if stream_type == AVMediaType::AVMEDIA_TYPE_AUDIO {
            first_audio_stream = Some(i as isize);
        }
        if stream_type == AVMediaType::AVMEDIA_TYPE_VIDEO {
            first_video_stream = Some(i as isize);
        }
    }

    let first_audio_stream = first_audio_stream.unwrap();
    let first_video_stream = first_video_stream.unwrap();

    let audio_decoder = AudioDecoder::new(
        "audio_decoder".to_string(),
        &format_context,
        first_audio_stream,
    )
    .unwrap();

    let video_decoder = VideoDecoder::new(
        "video_decoder".to_string(),
        &format_context,
        first_video_stream,
    )
    .unwrap();

    let mut audio_graph = FilterGraph::new().unwrap();

    //  audio graph
    let audio_graph = {
        audio_graph
            .add_input_from_audio_decoder("source_audio", &audio_decoder)
            .unwrap();

        let mut parameters = HashMap::new();
        parameters.insert(
            "sample_rates".to_string(),
            ParameterValue::String("48000".to_string()),
        );
        parameters.insert(
            "channel_layouts".to_string(),
            ParameterValue::String("stereo".to_string()),
        );
        parameters.insert(
            "sample_fmts".to_string(),
            ParameterValue::String("s32".to_string()),
        );

        let filter = Filter {
            name: "aformat".to_string(),
            label: Some("Format audio samples".to_string()),
            parameters,
            inputs: None,
            outputs: None,
        };

        let filter = audio_graph.add_filter(&filter).unwrap();
        audio_graph.add_audio_output("main_audio").unwrap();

        audio_graph
            .connect_input("source_audio", 0, &filter, 0)
            .unwrap();
        audio_graph
            .connect_output(&filter, 0, "main_audio", 0)
            .unwrap();
        audio_graph.validate().unwrap();

        audio_graph
    };

    let video_graph = {
        let mut video_graph = FilterGraph::new().unwrap();
        video_graph
            .add_input_from_video_decoder("source_video", &video_decoder)
            .unwrap();

        let mut parameters = HashMap::new();
        parameters.insert(
            "pix_fmts".to_string(),
            ParameterValue::String("rgba".to_string()),
        );

        let filter = Filter {
            name: "format".to_string(),
            label: Some("Format video".to_string()),
            parameters,
            inputs: None,
            outputs: None,
        };

        let filter = video_graph.add_filter(&filter).unwrap();
        video_graph.add_video_output("main_video").unwrap();

        video_graph
            .connect_input("source_video", 0, &filter, 0)
            .unwrap();
        video_graph
            .connect_output(&filter, 0, "main_video", 0)
            .unwrap();
        video_graph.validate().unwrap();

        video_graph
    };

    let video_queue = Arc::new(Mutex::new(VecDeque::<(f64, Vec<u8>)>::new()));

    let video_queue_clone = video_queue.clone();

    // video thread
    std::thread::spawn(move || loop {
        let mut time_to_wait_inside = 0.0;
        let now = Instant::now();
        if let Some((time_to_wait, frame)) = video_queue_clone.lock().unwrap().pop_front() {
            video_sender.send(frame).unwrap();
            time_to_wait_inside = time_to_wait;
        }

        let elapsed = now.elapsed().as_micros() as u64;

        if time_to_wait_inside != 0.0 {
            let in_millis = (time_to_wait_inside * 1_000_000.0) as u64;
            let dur = Duration::from_micros((in_millis - elapsed) - 10250);
            println!("Waiting time: {}micros", dur.as_micros());
            std::thread::sleep(dur);
        }
    });

    let (_stream, stream_handle) =
        OutputStream::try_default().expect("cant find any audio drivers");
    let sink = rodio::Sink::try_new(&stream_handle).unwrap();

    // unsafe {
    //     av_seek_frame(
    //         format_context.format_context,
    //         first_video_stream as i32,
    //         200000,
    //         0,
    //     );
    // }

    loop {
        if video_queue.lock().unwrap().len() >= 10 && sink.len() >= 10 {
            continue;
        }

        let Ok(packet) = format_context.next_packet() else {
            break;
        };

        if packet.get_stream_index() == first_video_stream {
            let frame = video_decoder.decode(&packet).unwrap();
            let (_, frames) = video_graph.process(&[], &[frame]).unwrap();
            let frame = frames.first().unwrap();

            unsafe {
                let stream = format_context.get_stream(first_video_stream);
                let timebase = (*stream).time_base;
                let pts = av_q2d(timebase) * (*frame.frame).best_effort_timestamp as f64;
                let duration = av_q2d(timebase) * (*frame.frame).pkt_duration as f64;

                // calculate the timestamp of the next frame in seconds
                let next_pts = pts + duration;

                let wait_time_in_seconds = next_pts - pts;
                let size = (video_decoder.get_height() * (*frame.frame).linesize[0]) as usize;
                let rgba_data = slice::from_raw_parts((*frame.frame).data[0], size).to_vec();

                video_queue
                    .lock()
                    .unwrap()
                    .push_back((wait_time_in_seconds, rgba_data));
            }
        }

        if packet.get_stream_index() == first_audio_stream {
            let frame = audio_decoder.decode(&packet).unwrap();
            let (frames, _) = audio_graph.process(&[frame], &[]).unwrap();
            let frame = frames.first().unwrap();

            unsafe {
                let size = ((*frame.frame).channels * (*frame.frame).nb_samples) as usize;

                let data: Vec<i32> =
                    slice::from_raw_parts((*frame.frame).data[0] as _, size).to_vec();
                let float_samples: Vec<f32> = data
                    .iter()
                    .map(|value| (*value as f32) / i32::MAX as f32)
                    .collect();

                sink.append(SamplesBuffer::new(
                    (*frame.frame).channels as _,
                    (*frame.frame).sample_rate as _,
                    float_samples,
                ));
            }
        }
    }
}
