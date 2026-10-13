//! Live camera capture through FFmpeg's `avfoundation` input device (macOS).
//!
//! Frames go through the same `Shared::latest` slot as file clips, so the renderer treats
//! a camera like any other player. Failures never panic: they end the capture thread and
//! are reported through `Player::error()`.
//!
//! Device listing shells out to the `ffmpeg` binary (`-list_devices true`), because the
//! avfoundation device only prints its list to the FFmpeg log. If the binary is missing the
//! error says so; a device can still be opened by index ("0") or by exact name.

use crate::{Frame, Shared, frame_to_rgba};
use ffmpeg_next as ff;
use std::ffi::CString;
use std::ptr;
use std::sync::atomic::Ordering;

/// A video capture device as listed by avfoundation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureDevice {
    pub index: u32,
    pub name: String,
}

/// Parses `ffmpeg -f avfoundation -list_devices true -i ""` output, keeping only the video
/// devices (screen captures included, as FFmpeg lists them there).
pub fn parse_avfoundation_list(text: &str) -> Vec<CaptureDevice> {
    fn entry(line: &str) -> Option<(u32, String)> {
        let rest = line.strip_prefix('[')?;
        let (idx, name) = rest.split_once(']')?;
        let index = idx.trim().parse().ok()?;
        let name = name.trim();
        (!name.is_empty()).then(|| (index, name.to_string()))
    }
    let mut video = false;
    let mut out = vec![];
    for line in text.lines() {
        if line.contains("video devices") {
            video = true;
        } else if line.contains("audio devices") {
            video = false;
        } else if video {
            let l = line.trim();
            // Either "[0] Name" or "[AVFoundation indev @ 0x..] [0] Name".
            let parsed = entry(l).or_else(|| entry(l.split_once("] ")?.1.trim()));
            if let Some((index, name)) = parsed {
                out.push(CaptureDevice { index, name });
            }
        }
    }
    out
}

/// Lists the video capture devices. Errors when not on macOS or when `ffmpeg` is not on PATH.
pub fn list_devices() -> Result<Vec<CaptureDevice>, String> {
    if !cfg!(target_os = "macos") {
        return Err("Camera capture is only supported on macOS (avfoundation)".into());
    }
    let out = std::process::Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-f",
            "avfoundation",
            "-list_devices",
            "true",
            "-i",
            "",
        ])
        .output()
        .map_err(|e| format!("Cannot list cameras: the ffmpeg binary is not available ({e})"))?;
    // ffmpeg exits non-zero after listing; the list is on stderr.
    Ok(parse_avfoundation_list(&String::from_utf8_lossy(
        &out.stderr,
    )))
}

fn open_error(device: &str, e: ff::Error) -> String {
    format!(
        "Cannot open camera '{device}': {e}. If the camera exists, allow camera access for \
         this app in System Settings > Privacy & Security > Camera and try again"
    )
}

fn open_avfoundation(
    device: &str,
    framerate: Option<&str>,
) -> Result<ff::format::context::Input, ff::Error> {
    // "<video>:none" skips the audio device.
    let url = CString::new(format!("{device}:none")).map_err(|_| ff::Error::InvalidData)?;
    let mut opts = ff::Dictionary::new();
    if let Some(fr) = framerate {
        opts.set("framerate", fr);
    }
    unsafe {
        let fmt = ff::ffi::av_find_input_format(c"avfoundation".as_ptr());
        if fmt.is_null() {
            return Err(ff::Error::DemuxerNotFound);
        }
        let mut ps = ptr::null_mut();
        let mut raw = opts.disown();
        let r = ff::ffi::avformat_open_input(&mut ps, url.as_ptr(), fmt, &mut raw);
        drop(ff::Dictionary::own(raw));
        if r < 0 {
            return Err(ff::Error::from(r));
        }
        let r = ff::ffi::avformat_find_stream_info(ps, ptr::null_mut());
        if r < 0 {
            ff::ffi::avformat_close_input(&mut ps);
            return Err(ff::Error::from(r));
        }
        Ok(ff::format::context::Input::wrap(ps))
    }
}

/// Capture loop: publishes RGBA frames until `shared.stop`, a read error or a decode setup
/// error. Returns the message to show on failure.
pub(crate) fn run_capture(device: &str, shared: &Shared) -> Result<(), String> {
    if !cfg!(target_os = "macos") {
        return Err("Camera capture is only supported on macOS (avfoundation)".into());
    }
    ff::init().map_err(|e| e.to_string())?;
    let device = if device.trim().is_empty() {
        "0"
    } else {
        device.trim()
    };
    // Some cameras reject a fixed rate: retry with the device default.
    let mut ictx = open_avfoundation(device, Some("30"))
        .or_else(|_| open_avfoundation(device, None))
        .map_err(|e| open_error(device, e))?;
    let (idx, params) = {
        let st = ictx
            .streams()
            .best(ff::media::Type::Video)
            .ok_or_else(|| format!("Camera '{device}' has no video stream"))?;
        (st.index(), st.parameters())
    };
    let mut decoder = ff::codec::context::Context::from_parameters(params)
        .and_then(|c| c.decoder().video())
        .map_err(|e| format!("Cannot decode camera '{device}': {e}"))?;
    let mut scaler = None;
    let mut seq = 0u64;
    let mut decoded = ff::frame::Video::empty();
    while !shared.stop.load(Ordering::Relaxed) {
        let mut pkt = ff::Packet::empty();
        pkt.read(&mut ictx)
            .map_err(|e| format!("Camera '{device}' disconnected or stopped: {e}"))?;
        if pkt.stream() != idx || decoder.send_packet(&pkt).is_err() {
            continue;
        }
        while decoder.receive_frame(&mut decoded).is_ok() {
            seq += 1;
            match frame_to_rgba(&mut scaler, &decoded, seq) {
                Ok(f) => publish(shared, f),
                Err(e) => eprintln!("vjc-media: camera frame skipped: {e}"),
            }
        }
    }
    Ok(())
}

fn publish(shared: &Shared, f: Frame) {
    *shared.latest.lock().unwrap() = Some(f);
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
[AVFoundation indev @ 0x7f8] AVFoundation video devices:
[AVFoundation indev @ 0x7f8] [0] FaceTime HD Camera
[AVFoundation indev @ 0x7f8] [1] Capture screen 0
[AVFoundation indev @ 0x7f8] AVFoundation audio devices:
[AVFoundation indev @ 0x7f8] [0] MacBook Pro Microphone
: Input/output error
";

    #[test]
    fn parses_video_devices_only() {
        let d = parse_avfoundation_list(SAMPLE);
        assert_eq!(
            d,
            vec![
                CaptureDevice {
                    index: 0,
                    name: "FaceTime HD Camera".into()
                },
                CaptureDevice {
                    index: 1,
                    name: "Capture screen 0".into()
                },
            ]
        );
    }

    #[test]
    fn parses_unprefixed_lines_and_empty_input() {
        let d = parse_avfoundation_list("AVFoundation video devices:\n[2] USB Cam\n");
        assert_eq!(d.len(), 1);
        assert_eq!((d[0].index, d[0].name.as_str()), (2, "USB Cam"));
        assert!(parse_avfoundation_list("").is_empty());
        assert!(parse_avfoundation_list("garbage [0] x").is_empty());
    }

    #[test]
    #[ignore = "opens avfoundation; may prompt for camera permission"]
    fn missing_device_reports_error_without_panic() {
        let shared = Shared::new();
        assert!(run_capture("no-such-camera-xyz", &shared).is_err());
    }

    #[test]
    #[ignore = "needs a camera and macOS camera permission"]
    fn real_camera_delivers_a_frame() {
        let p = crate::Player::open_capture("0");
        for _ in 0..100 {
            if p.latest_frame().is_some() || p.error().is_some() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        assert!(p.latest_frame().is_some(), "{:?}", p.error());
    }
}
