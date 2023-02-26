use anyhow::Error;
use crossbeam_channel::Sender;
use gst::{element_warning, prelude::*};

pub struct MediaDecoder;

impl MediaDecoder {
    pub fn new(path_or_url: &str, new_frame_callback: Sender<Vec<u8>>) -> Result<Self, Error> {
        gst::init()?;

        let pipeline = gst::Pipeline::default();
        let decodebin = gst::ElementFactory::make("uridecodebin")
            .property(
                "uri",
                r#"http://192.168.178.48:32400/library/parts/1386/1676086764/file.mkv?download=1&X-Plex-Token=pz-yPEh-emTJ_VEb71CS"#,
            )
            .build()?;
        // let decodebin = gst::ElementFactory::make("decodebin").build()?;

        pipeline.add_many(&[&decodebin])?;
        gst::Element::link_many(&[&decodebin])?;

        // Need to move a new reference into the closure.
        // !!ATTENTION!!:
        // It might seem appealing to use pipeline.clone() here, because that greatly
        // simplifies the code within the callback. What this actually does, however, is creating
        // a memory leak. The clone of a pipeline is a new strong reference on the pipeline.
        // Storing this strong reference of the pipeline within the callback (we are moving it in!),
        // which is in turn stored in another strong reference on the pipeline is creating a
        // reference cycle.
        // DO NOT USE pipeline.clone() TO USE THE PIPELINE WITHIN A CALLBACK
        let pipeline_weak = pipeline.downgrade();
        // Connect to decodebin's pad-added signal, that is emitted whenever
        // it found another stream from the input file and found a way to decode it to its raw format.
        // decodebin automatically adds a src-pad for this raw stream, which
        // we can use to build the follow-up pipeline.
        decodebin.connect_pad_added(move |dbin, src_pad| {
            // Here we temporarily retrieve a strong reference on the pipeline from the weak one
            // we moved into this callback.
            let pipeline = match pipeline_weak.upgrade() {
                Some(pipeline) => pipeline,
                None => return,
            };

            // Try to detect whether the raw stream decodebin provided us with
            // just now is either audio or video (or none of both, e.g. subtitles).
            let (is_audio, is_video) = {
                let media_type = src_pad.current_caps().and_then(|caps| {
                    caps.structure(0).map(|s| {
                        let name = s.name();
                        (name.starts_with("audio/"), name.starts_with("video/"))
                    })
                });

                match media_type {
                    None => {
                        element_warning!(
                            dbin,
                            gst::CoreError::Negotiation,
                            ("Failed to get media type from pad {}", src_pad.name())
                        );

                        return;
                    }
                    Some(media_type) => media_type,
                }
            };

            // We create a closure here, calling it directly below it, because this greatly
            // improves readability for error-handling. Like this, we can simply use the
            // ?-operator within the closure, and handle the actual error down below where
            // we call the insert_sink(..) closure.
            let insert_sink = |is_audio, is_video| -> Result<(), Error> {
                if is_audio {
                    // decodebin found a raw audiostream, so we build the follow-up pipeline to
                    // play it on the default audio playback device (using autoaudiosink).
                    let queue = gst::ElementFactory::make("queue").build()?;
                    let convert = gst::ElementFactory::make("audioconvert").build()?;
                    let resample = gst::ElementFactory::make("audioresample").build()?;
                    let sink = gst::ElementFactory::make("autoaudiosink").build()?;

                    let elements = &[&queue, &convert, &resample, &sink];
                    pipeline.add_many(elements)?;
                    gst::Element::link_many(elements)?;

                    // !!ATTENTION!!:
                    // This is quite important and people forget it often. Without making sure that
                    // the new elements have the same state as the pipeline, things will fail later.
                    // They would still be in Null state and can't process data.
                    for e in elements {
                        e.sync_state_with_parent()?;
                    }

                    // Get the queue element's sink pad and link the decodebin's newly created
                    // src pad for the audio stream to it.
                    let sink_pad = queue.static_pad("sink").expect("queue has no sinkpad");
                    src_pad.link(&sink_pad)?;
                } else if is_video {
                    //     // decodebin found a raw videostream, so we build the follow-up pipeline to
                    //     // display it using the autovideosink.
                    let queue = gst::ElementFactory::make("queue").build()?;
                    let convert = gst::ElementFactory::make("videoconvert").build()?;
                    let scale = gst::ElementFactory::make("videoscale").build()?;
                    let appsink = gst_app::AppSink::builder()
                        .caps(
                            &gst::Caps::builder("video/x-raw")
                                .field("format", "RGBA")
                                .build(),
                        )
                        .build();

                    let elements = &[&queue, &convert, &scale, (appsink.upcast_ref())];
                    pipeline.add_many(elements)?;
                    gst::Element::link_many(elements)?;

                    for e in elements {
                        e.sync_state_with_parent()?
                    }

                    let cl = new_frame_callback.clone();
                    appsink.set_callbacks(
                        gst_app::AppSinkCallbacks::builder()
                            .new_sample(move |appsink| {
                                let sample =
                                    appsink.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                                let buffer = sample.buffer().unwrap();

                                let map = buffer.map_readable().unwrap();

                                let data = map.as_slice();

                                cl.send(data.to_vec()).unwrap();
                                Ok(gst::FlowSuccess::Ok)
                            })
                            .build(),
                    );
                    // Get the queue element's sink pad and link the decodebin's newly created
                    // src pad for the video stream to it.
                    let sink_pad = queue.static_pad("sink").expect("queue has no sinkpad");
                    src_pad.link(&sink_pad)?;
                }

                Ok(())
            };

            // When adding and linking new elements in a callback fails, error information is often sparse.
            // GStreamer's built-in debugging can be hard to link back to the exact position within the code
            // that failed. Since callbacks are called from random threads within the pipeline, it can get hard
            // to get good error information. The macros used in the following can solve that. With the use
            // of those, one can send arbitrary rust types (using the pipeline's bus) into the mainloop.
            // What we send here is unpacked down below, in the iteration-code over sent bus-messages.
            // Because we are using the failure crate for error details here, we even get a backtrace for
            // where the error was constructed. (If RUST_BACKTRACE=1 is set)
            if let Err(err) = insert_sink(is_audio, is_video) {
                // The following sends a message of type Error on the bus, containing our detailed
                // error information.
                println!("Something went wrong {:?}", err);
                // element_error!(
                //     dbin,
                //     gst::LibraryError::Failed,
                //     ("Failed to insert sink"),
                //     details: gst::Structure::builder("error-details")
                //                 .field("error",
                //                        &ErrorValue(Arc::new(Mutex::new(Some(err)))))
                //                 .build()
                // );
            }
        });

        pipeline.set_state(gst::State::Playing)?;

        let bus = pipeline.bus().unwrap();
        for msg in bus.iter_timed(gst::ClockTime::NONE) {
            use gst::MessageView;
            match msg.view() {
                MessageView::Eos(..) => {
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
                _ => (),
            }
        }

        pipeline.set_state(gstreamer::State::Null)?;

        Ok(Self)
    }
}
