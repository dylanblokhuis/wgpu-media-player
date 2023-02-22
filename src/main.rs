use media_decoder::MediaDecoder;
use renderer::{VideoRenderer, INDICES};

use std::{
    sync::{Arc, Mutex},
    u8,
};
use tokio::sync::oneshot;
use winit::{
    dpi::PhysicalSize,
    event::{Event, WindowEvent},
    event_loop::{ControlFlow, EventLoopBuilder},
};

mod app;
mod media_decoder;
mod renderer;
mod texture;

#[derive(Debug)]
enum UserEvent {
    NewFrameReady(Vec<u8>),
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
    let window = winit::window::WindowBuilder::new()
        .with_inner_size(winit::dpi::LogicalSize::new(1280, 720))
        .with_title("wgpu-media-player")
        .build(&event_loop)
        .unwrap();

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

    let config = wgpu::SurfaceConfiguration {
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        format: swapchain_format,
        width: size.width,
        height: size.height,
        present_mode: wgpu::PresentMode::Fifo,
        alpha_mode: wgpu::CompositeAlphaMode::Auto,
        view_formats: [swapchain_format].to_vec(),
    };

    surface.configure(&device, &config);

    let repaint_proxy = Arc::new(Mutex::new(event_loop.create_proxy()));
    let (video_size_sender, video_size_receiver) = oneshot::channel::<PhysicalSize<u32>>();
    let (load_file_sender, load_file_receiver) = oneshot::channel::<String>();

    std::thread::spawn(move || {
        let path = load_file_receiver.blocking_recv().unwrap();

        let mut media_decoder = MediaDecoder::new(&path, move |frame| {
            repaint_proxy
                .lock()
                .unwrap()
                .send_event(UserEvent::NewFrameReady(frame))
                .unwrap();
        });

        video_size_sender
            .send(media_decoder.get_video_size())
            .unwrap();

        media_decoder.start();
    });

    let device = Arc::new(device);
    let config = Arc::new(Mutex::new(config));
    let renderer = Arc::new(Mutex::new(None));

    {
        let device = device.clone();
        let config = config.clone();
        let renderer = renderer.clone();
        let window_inner_size = window.inner_size();
        std::thread::spawn(move || {
            let size = video_size_receiver.blocking_recv().unwrap();
            *renderer.lock().unwrap() = Some(VideoRenderer::new(
                window_inner_size,
                size,
                device,
                config.lock().unwrap().clone(),
            ));
        });
    }
    let mut app = app::App::new();
    app.set_on_load_file_request(move |path| {
        load_file_sender.send(path).unwrap();
    });

    event_loop.run(move |event, _, control_flow| {
        // Have the closure take ownership of the resources.
        // `event_loop.run` never returns, therefore we must do this to ensure
        // the resources are properly cleaned up.
        let _ = (&instance, &adapter);

        *control_flow = ControlFlow::Wait;

        match event {
            Event::WindowEvent { event, .. } => {
                if matches!(event, WindowEvent::CloseRequested | WindowEvent::Destroyed) {
                    *control_flow = ControlFlow::Exit;
                }

                if let WindowEvent::Resized(size) = &event {
                    config.lock().unwrap().width = size.width;
                    config.lock().unwrap().height = size.height;
                    surface.configure(&device, &config.lock().unwrap());

                    if let Some(renderer) = renderer.lock().unwrap().as_mut() {
                        renderer.handle_resize(&device, *size);
                    }

                    // On macos the window needs to be redrawn manually after resizing
                    window.request_redraw();
                } else if let WindowEvent::ScaleFactorChanged {
                    new_inner_size: size,
                    ..
                } = &event
                {
                    config.lock().unwrap().width = size.width;
                    config.lock().unwrap().height = size.height;
                    surface.configure(&device, &config.lock().unwrap());

                    if let Some(renderer) = renderer.lock().unwrap().as_mut() {
                        renderer.handle_resize(&device, **size);
                    }

                    // On macos the window needs to be redrawn manually after resizing
                    window.request_redraw();
                }

                // if let WindowEvent::DroppedFile(path) = &event {
                //     if let Some(load_file_sender) = load_file_sender.take() {
                //         load_file_sender
                //             .send(
                //                 path.to_str()
                //                     .expect("Failed to convert path to string")
                //                     .to_string(),
                //             )
                //             .unwrap();
                //     }
                // }

                app.handle_window_event(&event);
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

                    if let Some(renderer) = renderer.lock().unwrap().as_mut() {
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
                }

                queue.submit(Some(encoder.finish()));
                frame.present();
            }
            Event::UserEvent(UserEvent::NewFrameReady(data)) => {
                if let Some(renderer) = renderer.lock().unwrap().as_mut() {
                    renderer.new_frame(&queue, &data);
                }
                window.request_redraw();
            }
            _ => {}
        }
    });
}
