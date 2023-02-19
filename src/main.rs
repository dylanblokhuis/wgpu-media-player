use cpal::{traits::StreamTrait, ChannelCount, SampleRate, Stream};
use crossbeam_channel::Sender;
use renderer::{VideoRenderer, INDICES};
use ringbuf::{HeapConsumer, HeapRb};
use stainless_ffmpeg::prelude::FormatContext;
use stainless_ffmpeg::prelude::*;
use std::{collections::HashMap, slice, u8};
use winit::{
    event::{Event, WindowEvent},
    event_loop::{ControlFlow, EventLoop, EventLoopBuilder},
    window::Window,
};

mod renderer;
mod texture;
// Since the Rust time-functions `Duration` and `Instant` work with nanoseconds
// by default, it is much simpler to convert a PTS to nanoseconds,
// that is why the following constant has been added.
const ONE_NANOSECOND: i64 = 1000000000;

fn main() {
    let event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
    let window = winit::window::Window::new(&event_loop).unwrap();
    window.set_inner_size(winit::dpi::LogicalSize::new(1280, 900));

    pollster::block_on(run(event_loop, window));
}

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
                limits: wgpu::Limits::default(),
            },
            None,
        )
        .await
        .expect("Failed to create device");

    let swapchain_capabilities = surface.get_capabilities(&adapter);
    let swapchain_format = swapchain_capabilities.formats[0];

    let mut config = wgpu::SurfaceConfiguration {
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        format: swapchain_format,
        width: size.width,
        height: size.height,
        present_mode: wgpu::PresentMode::Fifo,
        alpha_mode: wgpu::CompositeAlphaMode::Auto,
        view_formats: [swapchain_format].to_vec(),
    };

    surface.configure(&device, &config);

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

    let mut renderer = VideoRenderer::new(size, &device, &config);

    event_loop.run(move |event, _, control_flow| {
        // Have the closure take ownership of the resources.
        // `event_loop.run` never returns, therefore we must do this to ensure
        // the resources are properly cleaned up.
        let _ = (&instance, &adapter);

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

                renderer.handle_resize(&device, size);

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
                                load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                                store: true,
                            },
                        })],
                        depth_stencil_attachment: None,
                    });

                    // im not going to bother -> https://github.com/gfx-rs/wgpu/issues/1453
                    render_pass.set_pipeline(&renderer.render_pipeline);
                    render_pass.set_bind_group(0, &renderer.bind_group, &[]);
                    render_pass.set_vertex_buffer(0, renderer.vertex_buffer.slice(..));
                    render_pass.set_index_buffer(
                        renderer.index_buffer.slice(..),
                        wgpu::IndexFormat::Uint16,
                    );
                    render_pass.draw_indexed(0..INDICES.len() as u32, 0, 0..1);
                }

                queue.submit(Some(encoder.finish()));
                frame.present();
            }
            Event::UserEvent(UserEvent::NewFrameReady(data)) => {
                renderer.new_frame(&queue, &data);
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

    let resample_rate = 48000;
    let channels = 2;

    //  audio graph
    let audio_graph = {
        audio_graph
            .add_input_from_audio_decoder("source_audio", &audio_decoder)
            .unwrap();

        let mut parameters = HashMap::new();
        parameters.insert(
            "sample_rates".to_string(),
            ParameterValue::String(resample_rate.to_string()),
        );
        parameters.insert(
            "channel_layouts".to_string(),
            ParameterValue::String(if channels == 1 {
                "mono".to_string()
            } else {
                "stereo".to_string()
            }),
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

    let (mut video_producer, mut video_consumer) = HeapRb::<(i64, Vec<u8>)>::new(50).split();
    let (mut audio_producer, audio_consumer) = HeapRb::<f32>::new(50 * 1024 * 1024).split();

    std::thread::spawn(move || {
        let mut prev_pts = None;
        let mut now = std::time::Instant::now();

        loop {
            if let Some((pts, frame)) = video_consumer.pop() {
                if let Some(prev) = prev_pts {
                    let elapsed = now.elapsed();
                    if pts > prev {
                        let sleep_time = std::time::Duration::new(0, (pts - prev) as u32);
                        if elapsed < sleep_time {
                            println!("sleeping for {:?}", sleep_time - elapsed);
                            spin_sleep::sleep(sleep_time - elapsed);
                        }
                    }
                }

                prev_pts = Some(pts);
                now = std::time::Instant::now();

                video_sender.send(frame).unwrap();
            }
        }
    });

    let stream = audio_player(audio_consumer, channels, SampleRate(resample_rate as u32));
    stream.play().unwrap();

    // unsafe {
    //     av_seek_frame(
    //         format_context.format_context,
    //         first_video_stream as i32,
    //         600000,
    //         0,
    //     );
    // }

    loop {
        if video_producer.len() >= 50 {
            continue;
        }

        let Ok(packet) = format_context.next_packet() else {
            break;
        };

        if packet.get_stream_index() == first_video_stream {
            let Ok(frame) = video_decoder.decode(&packet) else {
                continue;
            };
            let (_, frames) = video_graph.process(&[], &[frame]).unwrap();
            let frame = frames.first().unwrap();

            unsafe {
                let stream = (*format_context.get_stream(first_video_stream)).time_base;
                let pts_nano = av_rescale_q(
                    (*frame.frame).best_effort_timestamp,
                    stream,
                    av_make_q(1, ONE_NANOSECOND as i32),
                );

                let size = (video_decoder.get_height() * (*frame.frame).linesize[0]) as usize;
                let rgba_data = slice::from_raw_parts((*frame.frame).data[0], size).to_vec();
                video_producer.push((pts_nano, rgba_data)).unwrap();
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

                audio_producer.push_slice(&float_samples);
            }
        }
    }
}

fn audio_player(
    mut audio_consumer: HeapConsumer<f32>,
    channels: ChannelCount,
    sample_rate: SampleRate,
) -> Stream {
    use cpal::traits::{DeviceTrait, HostTrait};

    let host = cpal::default_host();
    let device = host
        .default_output_device()
        .expect("no output device available");

    let mut supported_configs_range = device
        .supported_output_configs()
        .expect("error while querying configs");

    let supported_config = supported_configs_range
        .find(|config| {
            config.channels() == channels
                && sample_rate >= config.min_sample_rate()
                && sample_rate <= config.max_sample_rate()
                && config.sample_format() == cpal::SampleFormat::F32
        })
        .expect("no supported config?!")
        .with_sample_rate(sample_rate);

    let config = supported_config.into();

    device
        .build_output_stream(
            &config,
            move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                audio_consumer.pop_slice(data);
            },
            move |err| println!("CPAL error: {:?}", err),
            None,
        )
        .unwrap()
}
