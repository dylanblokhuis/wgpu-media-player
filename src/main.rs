use crossbeam_channel::Sender;
use rodio::OutputStream;
use rusty_ffmpeg::ffi::*;
use std::{
    ffi::{c_void, CStr, CString},
    num::NonZeroU32,
    ptr::null_mut,
    slice,
    time::Duration,
};
use wgpu::util::DeviceExt;
use winit::{
    event::{Event, WindowEvent},
    event_loop::{ControlFlow, EventLoop, EventLoopBuilder},
    window::Window,
};

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
    let (sender, receiver) = crossbeam_channel::bounded::<Vec<u8>>(1);

    std::thread::spawn(move || {
        main_ffmpeg(sender);
    });

    let event_proxy = event_loop.create_proxy();
    std::thread::spawn(move || loop {
        event_proxy
            .send_event(UserEvent::NewFrameReady(receiver.clone().recv().unwrap()))
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

fn main_ffmpeg(sender: Sender<Vec<u8>>) {
    unsafe {
        let mut av_f_ctx = avformat_alloc_context();

        let url = CString::new("/home/dylan/wgplayer/saga.mkv").unwrap();

        avformat_open_input(&mut av_f_ctx, url.as_ptr(), null_mut(), null_mut());

        let format_context = av_f_ctx.as_mut().unwrap();

        let format_name = CStr::from_ptr((*format_context.iformat).name)
            .to_str()
            .unwrap();

        println!(
            "format {}, duration {} us, bit_rate {}",
            format_name, format_context.duration, format_context.bit_rate
        );

        avformat_find_stream_info(format_context, null_mut());

        let mut video_stream = None;
        let mut audio_stream = None;
        let mut video_params = None;
        let mut audio_params = None;
        let mut video_codec = None;
        let mut audio_codec = None;

        for i in 0..format_context.nb_streams {
            let stream = *format_context.streams.offset(i as isize);
            let _params = (*stream).codecpar;

            if (*_params).codec_type == AVMediaType_AVMEDIA_TYPE_VIDEO && video_stream.is_none() {
                println!(
                    "video width: {}, height: {}",
                    (*_params).width,
                    (*_params).height
                );
                video_stream = Some(i);
                video_codec = Some(avcodec_find_decoder((*_params).codec_id));
                video_params = Some(_params);
            }

            if (*_params).codec_type == AVMediaType_AVMEDIA_TYPE_AUDIO && audio_stream.is_none() {
                println!(
                    "audio channels: {}, sample rate {}",
                    (*_params).codec_id,
                    (*_params).sample_rate
                );
                audio_stream = Some(i);
                audio_codec = Some(avcodec_find_decoder((*_params).codec_id));
                audio_params = Some(_params);
            }
        }

        let Some(video_stream) = video_stream else {
            panic!("video stream not found");
        };

        let Some(video_codec) = video_codec else {
            panic!("video codec not found");
        };

        let Some(audio_codec) = audio_codec else {
            panic!("audio codec not found");
        };

        let Some(video_params) = video_params else {
            panic!("video params not found");
        };
        let Some(audio_params) = audio_params else {
            panic!("audio params not found");
        };

        let audio_codec_ctx = avcodec_alloc_context3(audio_codec).as_mut().unwrap();
        let ret = avcodec_parameters_to_context(audio_codec_ctx, audio_params);
        if ret != 0 {
            panic!("failed to set audio params");
        }
        avcodec_open2(audio_codec_ctx, audio_codec, null_mut());

        let video_codec_ctx = avcodec_alloc_context3(video_codec).as_mut().unwrap();
        avcodec_parameters_to_context(video_codec_ctx, video_params);
        avcodec_open2(video_codec_ctx, video_codec, null_mut());

        let audio_frame = av_frame_alloc().as_mut().unwrap();
        let frame = av_frame_alloc().as_mut().unwrap();
        let frame_rgb = av_frame_alloc().as_mut().unwrap();

        let num_bytes = av_image_get_buffer_size(
            AVPixelFormat_AV_PIX_FMT_RGBA,
            video_codec_ctx.width,
            video_codec_ctx.height,
            32,
        );
        let buffer = av_malloc(num_bytes as u64) as *mut u8;

        av_image_fill_arrays(
            frame_rgb.data.as_mut_ptr(),
            frame_rgb.linesize.as_mut_ptr(),
            buffer,
            AVPixelFormat_AV_PIX_FMT_RGBA,
            video_codec_ctx.width,
            video_codec_ctx.height,
            32,
        );

        let packet = av_packet_alloc();

        let sws_context = sws_getContext(
            video_codec_ctx.width,
            video_codec_ctx.height,
            video_codec_ctx.pix_fmt,
            video_codec_ctx.width,
            video_codec_ctx.height,
            AVPixelFormat_AV_PIX_FMT_RGBA,
            SWS_BILINEAR as i32,
            null_mut(),
            null_mut(),
            null_mut(),
        );

        let maybe_audio_driver = OutputStream::try_default();

        while av_read_frame(format_context, packet) >= 0 {
            if (*packet).stream_index == video_stream as i32 {
                let ret = avcodec_send_packet(video_codec_ctx, packet);

                if ret < 0 {
                    panic!("Error sending a packet for decoding");
                }

                loop {
                    let ret = avcodec_receive_frame(video_codec_ctx, frame);
                    if ret == AVERROR_EOF || ret == AVERROR(EAGAIN) {
                        break;
                    } else if ret < 0 {
                        panic!("Error during decoding");
                    }

                    sws_scale(
                        sws_context,
                        frame.data.as_ptr() as *const *const u8,
                        frame.linesize.as_ptr(),
                        0,
                        video_codec_ctx.height,
                        frame_rgb.data.as_mut_ptr(),
                        frame_rgb.linesize.as_mut_ptr(),
                    );

                    let stream = format_context.streams.offset(video_stream as isize);
                    let fps = av_q2d((*(*stream)).r_frame_rate);
                    let sleep_time = (1.0 / fps) * 1000.0;

                    std::thread::sleep(Duration::from_millis(sleep_time as u64));

                    println!(
                        "Frame {} (nr={})",
                        av_get_picture_type_char(frame.pict_type),
                        video_codec_ctx.frame_number,
                    );

                    sender
                        .send(
                            slice::from_raw_parts(
                                frame_rgb.data[0],
                                (video_codec_ctx.height * frame_rgb.linesize[0]) as usize,
                            )
                            .to_vec(),
                        )
                        .unwrap();
                }
            }

            if (*packet).stream_index == audio_stream.unwrap() as i32 {
                let av_frame = av_frame_alloc();
                let mut ret = avcodec_receive_frame(audio_codec_ctx, av_frame);
                let mut got_frame = 0;
                let mut len1 = 0;
                if ret == 0 {
                    got_frame = 1;
                }
                if ret == AVERROR(EAGAIN) {
                    ret = 0;
                }
                if ret == 0 {
                    ret = avcodec_send_packet(audio_codec_ctx, packet);
                }
                if ret == AVERROR(EAGAIN) {
                    ret = 0;
                } else if ret < 0 {
                    panic!("Error during decoding");
                } else {
                    len1 = (*packet).size;
                }

                if len1 < 0 {
                    println!("Yo");
                    break;
                }

                // play audio?
                println!("audio frame {}", (*av_frame).nb_samples);

                // if ret == 0 {
                //     ret = avcodec_send_packet(audio_codec_ctx, packet);
                // }
                // if ret == AVERROR(EAGAIN) {
                //     ret = 0;

                // loop {
                //     if ret == AVERROR_EOF || ret == AVERROR(EAGAIN) {
                //         break;
                //     } else if ret < 0 {
                //         panic!("Error during decoding");
                //     }

                //     println!("audio frame {}", frame.nb_samples);
                // }
            }

            av_packet_unref(packet);
        }

        // cleanup
        av_free(buffer as *mut c_void);
        av_frame_free(frame_rgb as *mut AVFrame as *mut *mut AVFrame);
        av_free(frame_rgb as *mut AVFrame as *mut c_void);

        // Free the YUV frame
        av_frame_free(frame as *mut AVFrame as *mut *mut AVFrame);
        av_free(frame as *mut AVFrame as *mut c_void);

        // Close the codecs
        avcodec_close(video_codec_ctx);

        // Close the video file
        avformat_close_input(av_f_ctx as *mut *mut AVFormatContext);
    }
}
