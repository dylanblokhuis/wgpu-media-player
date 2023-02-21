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

    let repaint_proxy = Arc::new(Mutex::new(event_loop.create_proxy()));
    let (video_size_sender, video_size_receiver) = oneshot::channel::<PhysicalSize<u32>>();

    std::thread::spawn(move || {
        let path = std::env::args().nth(1).expect("No file provided");
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

    let mut renderer = Some(VideoRenderer::new(
        window.inner_size(),
        video_size_receiver.await.unwrap(),
        &device,
        &config,
    ));

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

                if let Some(renderer) = renderer.as_mut() {
                    renderer.handle_resize(&device, size);
                }

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

                    if let Some(renderer) = renderer.as_mut() {
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
                renderer.as_mut().unwrap().new_frame(&queue, &data);
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
