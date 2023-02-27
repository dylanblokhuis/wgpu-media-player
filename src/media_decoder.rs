use anyhow::Error;
use crossbeam_channel::Sender;
use gst::prelude::*;
use gstreamer_video::VideoInfo;

pub struct MediaDecoder;

impl MediaDecoder {
    pub fn new(
        path_or_url: &str,
        video_info_sender: Sender<VideoInfo>,
        new_frame_sender: Sender<Vec<u8>>,
    ) -> Result<Self, Error> {
        gst::init()?;

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

        let pipeline = gst::ElementFactory::make("playbin")
            .property("uri", path_or_url)
            .property("video-sink", &videosink)
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
