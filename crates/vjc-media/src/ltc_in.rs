//! Audio input for LTC: opens one channel of an input device with cpal and hands mono f32
//! samples to a sink. The stream is not `Send`, so it lives on its own thread until the
//! `LtcInput` is dropped. Needs a real audio device (not covered by CI tests).

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::sync::{Arc, Mutex, mpsc};

/// Names of the audio input devices.
pub fn input_devices() -> Vec<String> {
    let Ok(devs) = cpal::default_host().input_devices() else {
        return vec![];
    };
    devs.filter_map(|d| d.description().ok().map(|d| d.name().to_owned()))
        .collect()
}

/// Progress of opening the input stream.
#[derive(Debug, Clone, PartialEq)]
pub enum LtcStatus {
    Opening,
    /// Running at this sample rate.
    Ok(u32),
    Failed(String),
}

/// An input stream opened in the background; dropping it stops capture (or cancels the
/// open still in progress).
pub struct LtcInput {
    stop: Option<mpsc::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
    status: Arc<Mutex<LtcStatus>>,
}

impl LtcInput {
    /// Starts opening `device` (empty = default input) and returns at once; poll `status`.
    /// Channel `channel` (0-based, clamped to the device's channels) goes to the sink made by
    /// `make_sink(sample_rate)`. The sink runs on the audio thread: keep it short.
    pub fn start<F>(
        device: &str,
        channel: u16,
        make_sink: impl FnOnce(u32) -> F + Send + 'static,
    ) -> LtcInput
    where
        F: FnMut(&[f32]) + Send + 'static,
    {
        let status = Arc::new(Mutex::new(LtcStatus::Opening));
        let (stop_tx, stop_rx) = mpsc::channel::<()>();
        let device = device.to_owned();
        let st = status.clone();
        let set = move |v: LtcStatus| *st.lock().unwrap_or_else(|e| e.into_inner()) = v;
        let spawned =
            std::thread::Builder::new()
                .name("vjc-ltc-in".into())
                .spawn(move || match open(&device, channel, make_sink) {
                    Ok((stream, rate)) => {
                        set(LtcStatus::Ok(rate));
                        let _ = stop_rx.recv();
                        drop(stream);
                    }
                    Err(e) => set(LtcStatus::Failed(e)),
                });
        match spawned {
            Ok(t) => LtcInput {
                stop: Some(stop_tx),
                thread: Some(t),
                status,
            },
            Err(e) => LtcInput {
                stop: None,
                thread: None,
                status: Arc::new(Mutex::new(LtcStatus::Failed(e.to_string()))),
            },
        }
    }

    pub fn status(&self) -> LtcStatus {
        self.status
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
}

impl Drop for LtcInput {
    fn drop(&mut self) {
        drop(self.stop.take());
        // Never block the caller on a device that is still opening: that thread ends by
        // itself once the open returns and the stop channel is found closed.
        if self.status() != LtcStatus::Opening
            && let Some(t) = self.thread.take()
        {
            let _ = t.join();
        }
    }
}

fn open<F>(
    name: &str,
    channel: u16,
    make_sink: impl FnOnce(u32) -> F,
) -> Result<(cpal::Stream, u32), String>
where
    F: FnMut(&[f32]) + Send + 'static,
{
    let host = cpal::default_host();
    let device = if name.is_empty() {
        host.default_input_device()
    } else {
        host.input_devices()
            .ok()
            .and_then(|mut d| d.find(|d| d.description().is_ok_and(|x| x.name() == name)))
    }
    .ok_or_else(|| format!("No audio input device '{name}'"))?;
    let supported = device
        .default_input_config()
        .map_err(|e| format!("LTC input config: {e}"))?;
    let cfg = supported.config();
    let rate = cfg.sample_rate;
    let ch = usize::from(cfg.channels).max(1);
    let pick = usize::from(channel).min(ch - 1);
    let sink = make_sink(rate);
    let stream = match supported.sample_format() {
        cpal::SampleFormat::F32 => build::<f32, F>(&device, cfg, ch, pick, sink),
        cpal::SampleFormat::I16 => build::<i16, F>(&device, cfg, ch, pick, sink),
        cpal::SampleFormat::U16 => build::<u16, F>(&device, cfg, ch, pick, sink),
        cpal::SampleFormat::I32 => build::<i32, F>(&device, cfg, ch, pick, sink),
        other => return Err(format!("Unsupported audio sample format {other}")),
    }
    .map_err(|e| format!("LTC input: {e}"))?;
    stream.play().map_err(|e| format!("LTC input: {e}"))?;
    Ok((stream, rate))
}

fn build<T, F>(
    device: &cpal::Device,
    cfg: cpal::StreamConfig,
    ch: usize,
    pick: usize,
    mut sink: F,
) -> Result<cpal::Stream, cpal::Error>
where
    T: cpal::SizedSample,
    f32: cpal::FromSample<T>,
    F: FnMut(&[f32]) + Send + 'static,
{
    let mut mono: Vec<f32> = Vec::new();
    device.build_input_stream(
        cfg,
        move |data: &[T], _: &cpal::InputCallbackInfo| {
            mono.clear();
            mono.extend(
                data.chunks_exact(ch)
                    .map(|f| <f32 as cpal::FromSample<T>>::from_sample_(f[pick])),
            );
            sink(&mono);
        },
        |e| eprintln!("vjc-media: LTC input error: {e}"),
        None,
    )
}
