//! Hardware video decode (docs/specs/hw-decode.md).
//!
//! macOS uses VideoToolbox. D3D11VA (Windows) and VAAPI (Linux) go through the same code,
//! selected by `cfg`; they compile but have not been run. Frames come back to system memory
//! (NV12/P010) and use the normal swscale path; zero-copy into wgpu is a follow-up.

use ffmpeg_next as ff;
use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering};

use ff::ffi::{AVHWDeviceType, AVPixelFormat};

#[cfg(target_os = "macos")]
const DEVICE: Option<AVHWDeviceType> = Some(AVHWDeviceType::AV_HWDEVICE_TYPE_VIDEOTOOLBOX);
#[cfg(target_os = "macos")]
const HW_FORMAT: AVPixelFormat = AVPixelFormat::AV_PIX_FMT_VIDEOTOOLBOX;

#[cfg(target_os = "windows")]
const DEVICE: Option<AVHWDeviceType> = Some(AVHWDeviceType::AV_HWDEVICE_TYPE_D3D11VA);
#[cfg(target_os = "windows")]
const HW_FORMAT: AVPixelFormat = AVPixelFormat::AV_PIX_FMT_D3D11;

#[cfg(all(unix, not(target_os = "macos")))]
const DEVICE: Option<AVHWDeviceType> = Some(AVHWDeviceType::AV_HWDEVICE_TYPE_VAAPI);
#[cfg(all(unix, not(target_os = "macos")))]
const HW_FORMAT: AVPixelFormat = AVPixelFormat::AV_PIX_FMT_VAAPI;

#[cfg(not(any(unix, windows)))]
const DEVICE: Option<AVHWDeviceType> = None;
#[cfg(not(any(unix, windows)))]
const HW_FORMAT: AVPixelFormat = AVPixelFormat::AV_PIX_FMT_NONE;

/// How a decoder is opened.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Hw {
    /// Software only.
    Off,
    /// Try the hardware device; fall back to software on any failure.
    On,
    /// Like `On` but the device creation fails (test hook for the fallback).
    #[cfg_attr(not(test), allow(dead_code))]
    ForceFail,
}

/// Decoder status shown on the clip: 0 unknown, 1 software, 2 hardware.
pub(crate) const ST_SW: u8 = 1;
pub(crate) const ST_HW: u8 = 2;

pub(crate) fn status_label(code: u8) -> Option<&'static str> {
    match code {
        ST_SW => Some("sw"),
        ST_HW => Some("hw"),
        _ => None,
    }
}

/// Simultaneous hardware sessions. A slot is held by the decoder using it.
#[derive(Default)]
pub(crate) struct HwSlots {
    active: AtomicUsize,
}

pub(crate) struct HwSlot<'a>(&'a HwSlots);

impl HwSlots {
    pub(crate) fn try_acquire(&self, cap: usize) -> Option<HwSlot<'_>> {
        let mut cur = self.active.load(Ordering::Relaxed);
        loop {
            if cur >= cap {
                return None;
            }
            match self.active.compare_exchange_weak(
                cur,
                cur + 1,
                Ordering::AcqRel,
                Ordering::Relaxed,
            ) {
                Ok(_) => return Some(HwSlot(self)),
                Err(c) => cur = c,
            }
        }
    }

    pub(crate) fn active(&self) -> usize {
        self.active.load(Ordering::Relaxed)
    }
}

impl Drop for HwSlot<'_> {
    fn drop(&mut self) {
        self.0.active.fetch_sub(1, Ordering::AcqRel);
    }
}

pub(crate) static SLOTS: HwSlots = HwSlots {
    active: AtomicUsize::new(0),
};

static MODE: AtomicU8 = AtomicU8::new(1); // 1 = Auto, 0 = Off
static CAP: AtomicUsize = AtomicUsize::new(8);

/// Sets the Video > "Hardware decoding" preference. Applies to clips opened afterwards.
pub fn configure(enabled: bool, max_sessions: usize) {
    MODE.store(u8::from(enabled), Ordering::Relaxed);
    CAP.store(max_sessions, Ordering::Relaxed);
}

/// Hardware sessions currently open.
pub fn active_sessions() -> usize {
    SLOTS.active()
}

/// What a new player should do, from the preference.
pub(crate) fn preferred() -> Hw {
    if MODE.load(Ordering::Relaxed) == 1 && DEVICE.is_some() {
        Hw::On
    } else {
        Hw::Off
    }
}

pub(crate) fn cap() -> usize {
    CAP.load(Ordering::Relaxed)
}

/// Picks the hardware surface format when the decoder offers it; otherwise the first
/// software format (that clip decodes in software).
unsafe extern "C" fn get_format(
    _ctx: *mut ff::ffi::AVCodecContext,
    fmts: *const AVPixelFormat,
) -> AVPixelFormat {
    unsafe {
        let mut p = fmts;
        while *p != AVPixelFormat::AV_PIX_FMT_NONE {
            if *p == HW_FORMAT {
                return HW_FORMAT;
            }
            p = p.add(1);
        }
        *fmts
    }
}

/// Attaches a hardware device to the codec context. False (nothing attached) when the
/// platform has none or the device cannot be created.
pub(crate) fn attach(ctx: &mut ff::codec::context::Context, hw: Hw) -> bool {
    let Some(kind) = DEVICE else { return false };
    if hw != Hw::On {
        return false;
    }
    unsafe {
        let mut dev: *mut ff::ffi::AVBufferRef = std::ptr::null_mut();
        let r = ff::ffi::av_hwdevice_ctx_create(
            &mut dev,
            kind,
            std::ptr::null(),
            std::ptr::null_mut(),
            0,
        );
        if r < 0 || dev.is_null() {
            return false;
        }
        let c = ctx.as_mut_ptr();
        (*c).hw_device_ctx = ff::ffi::av_buffer_ref(dev);
        (*c).get_format = Some(get_format);
        ff::ffi::av_buffer_unref(&mut dev);
        !(*c).hw_device_ctx.is_null()
    }
}

/// True when `f` is a hardware surface that needs a transfer.
pub(crate) fn is_hw_frame(f: &ff::frame::Video) -> bool {
    unsafe { (*f.as_ptr()).format == HW_FORMAT as i32 }
}

/// Copies a hardware surface into `dst` in system memory (NV12/P010), keeping the timing.
pub(crate) fn download(
    src: &ff::frame::Video,
    dst: &mut ff::frame::Video,
) -> Result<(), ff::Error> {
    unsafe {
        let r = ff::ffi::av_hwframe_transfer_data(dst.as_mut_ptr(), src.as_ptr(), 0);
        if r < 0 {
            return Err(ff::Error::from(r));
        }
        ff::ffi::av_frame_copy_props(dst.as_mut_ptr(), src.as_ptr());
    }
    Ok(())
}
