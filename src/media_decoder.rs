use anyhow::Error;
use byte_slice_cast::AsSliceOf;
use cpal::{traits::StreamTrait, Stream};
use crossbeam_channel::Sender;
use gst::prelude::*;
use gstreamer_video::VideoInfo;
use ringbuf::{HeapConsumer, HeapRb};

pub struct MediaDecoder;

impl MediaDecoder {
    pub fn new(
        path_or_url: &str,
        video_info_sender: Sender<VideoInfo>,
        new_frame_sender: Sender<Vec<u8>>,
    ) -> Result<Self, Error> {
        gst::init()?;

        let (mut audio_producer, audio_consumer) = HeapRb::new(50 * 1024 * 1024).split();
        let (channels, sample_rate, audio_stream) = setup_audio_stream(audio_consumer);
        audio_stream.play().unwrap();

        let videosink = gst_app::AppSink::builder()
            .caps(
                &gst::Caps::builder("video/x-raw")
                    .field("format", "RGBA")
                    .build(),
            )
            .build();

        let mut has_sent_info = false;

        videosink.set_callbacks(
            gst_app::AppSinkCallbacks::builder()
                .new_sample(move |appsink| {
                    let sample = appsink.pull_sample().map_err(|_| gst::FlowError::Eos)?;

                    if !has_sent_info {
                        let info = gst_video::VideoInfo::from_caps(sample.caps().unwrap()).unwrap();
                        video_info_sender.send(info).unwrap();
                        has_sent_info = true;
                    }

                    let buffer = sample.buffer().unwrap();
                    let map = buffer.map_readable().unwrap();
                    let data = map.as_slice();

                    new_frame_sender.send(data.to_vec()).unwrap();
                    Ok(gst::FlowSuccess::Ok)
                })
                .build(),
        );

        let audiosink = gst_app::AppSink::builder()
            .caps(
                &gst::Caps::builder("audio/x-raw")
                    .field("format", "F32LE")
                    .field("rate", sample_rate)
                    .field("channels", channels)
                    .build(),
            )
            .build();

        audiosink.set_callbacks(
            gst_app::AppSinkCallbacks::builder()
                .new_sample(move |appsink| {
                    let sample = appsink.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                    let buffer = sample.buffer().unwrap();
                    let map = buffer.map_readable().unwrap();
                    let samples = map.as_slice_of::<f32>().unwrap();
                    audio_producer.push_slice(samples);
                    Ok(gst::FlowSuccess::Ok)
                })
                .build(),
        );

        // audio_pipeline.add_many(&[&audio_convert, &audio_resample, audiosink.upcast_ref()])?;
        // gst::Element::link_many(&[&audio_convert, &audio_resample, audiosink.upcast_ref()])?;

        let pipeline = gst::ElementFactory::make("playbin")
            .property("uri", path_or_url)
            .property("video-sink", &videosink)
            .property("audio-sink", &audiosink)
            .build()?;

        let target_state = gst::State::Playing;

        pipeline.set_state(gst::State::Playing)?;

        let bus = pipeline.bus().unwrap();
        for msg in bus.iter_timed(gst::ClockTime::NONE) {
            use gst::MessageView;
            match msg.view() {
                MessageView::Eos(..) => {
                    pipeline.set_state(gst::State::Paused)?;
                    println!("received eos");
                    // An EndOfStream event was sent to the pipeline, so exit
                    break;
                }
                MessageView::Error(err) => {
                    println!(
                        "Error from {:?}: {} ({:?})",
                        err.src().map(|s| s.path_string()),
                        err.error(),
                        err.debug()
                    );
                    break;
                }
                MessageView::Buffering(msg) => {
                    let percent = msg.percent();
                    if percent < 100 && target_state >= gst::State::Paused {
                        println!("Buffering {}%", percent);
                        pipeline.set_state(gst::State::Paused)?;
                    } else if target_state >= gst::State::Playing {
                        pipeline.set_state(gst::State::Playing)?;
                    } else if target_state >= gst::State::Paused {
                        println!("Buffering complete");
                    }
                }
                MessageView::ClockLost(_) => {
                    if target_state >= gst::State::Playing {
                        pipeline.set_state(gst::State::Paused)?;
                        pipeline.set_state(gst::State::Playing)?;
                    }
                }
                _ => (),
            }
        }

        pipeline.set_state(gstreamer::State::Null)?;

        Ok(Self)
    }
}

fn setup_audio_stream(mut audio_consumer: HeapConsumer<f32>) -> (i32, i32, Stream) {
    use cpal::traits::{DeviceTrait, HostTrait};

    let host = cpal::default_host();
    let device = host
        .default_output_device()
        .expect("no output device available");

    let mut supported_configs_range = device
        .supported_output_configs()
        .expect("error while querying configs");

    let config = supported_configs_range
        .find(|_| true)
        .unwrap()
        .with_max_sample_rate();

    (
        config.channels() as i32,
        config.sample_rate().0 as i32,
        device
            .build_output_stream(
                &config.into(),
                move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                    audio_consumer.pop_slice(data);
                },
                move |err| println!("CPAL error: {:?}", err),
                None,
            )
            .unwrap(),
    )
}
