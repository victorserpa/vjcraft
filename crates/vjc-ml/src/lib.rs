//! Machine-learning effects: ONNX inference with tract on a worker thread per instance.
//!
//! Two models are supported, both looked up as files in a models folder (nothing is ever
//! downloaded by the app):
//! - MiDaS v2.1 small (MIT): monocular depth, one value per pixel, normalised to 0..255.
//! - Robust Video Matting, MobileNetV3 fp32 (GPL-3.0): alpha matte, with recurrent state
//!   carried from frame to frame.
//!
//! Nothing here touches the GPU. The render thread hands a reduced RGBA frame to a
//! `Worker` (`submit`, never blocks, a newer frame replaces an older unprocessed one) and
//! takes whatever mask finished last (`take`).

use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Instant;
use tract_onnx::prelude::*;

type Plan = Arc<TypedRunnableModel>;

/// Which model an effect runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ModelKind {
    /// MiDaS v2.1 small: depth (near = 255).
    Depth,
    /// Robust Video Matting: foreground alpha (person = 255).
    Matting,
}

impl ModelKind {
    pub fn file_name(self) -> &'static str {
        match self {
            ModelKind::Depth => "model-small.onnx",
            ModelKind::Matting => "rvm_mobilenetv3_fp32.onnx",
        }
    }

    /// Fixed network input size (width, height). The GPU reduces frames to exactly this.
    pub fn input_size(self) -> (u32, u32) {
        match self {
            ModelKind::Depth => (256, 256),
            // Multiples of 32 so every recurrent state has an integer size.
            ModelKind::Matting => (384, 224),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            ModelKind::Depth => "MiDaS small",
            ModelKind::Matting => "Robust Video Matting",
        }
    }

    pub fn path_in(self, dir: &Path) -> PathBuf {
        dir.join(self.file_name())
    }

    /// Message shown by an effect whose model file is missing.
    pub fn missing_message(self, dir: &Path) -> String {
        format!(
            "model not found: put {} in {} (download links: docs/ml.md)",
            self.file_name(),
            dir.display()
        )
    }
}

/// Result of one inference: one byte per pixel, `w * h` bytes, top row first.
#[derive(Debug, Clone, PartialEq)]
pub struct Mask {
    pub w: u32,
    pub h: u32,
    pub data: Vec<u8>,
}

// ---------------------------------------------------------------- preprocessing (pure)

const IMAGENET_MEAN: [f32; 3] = [0.485, 0.456, 0.406];
const IMAGENET_STD: [f32; 3] = [0.229, 0.224, 0.225];

/// Bilinear resize of an RGBA8 image (pixel-centre sampling, edges clamped).
pub fn resize_rgba(src: &[u8], (sw, sh): (u32, u32), (dw, dh): (u32, u32)) -> Vec<u8> {
    let (sw, sh, dw, dh) = (sw as usize, sh as usize, dw as usize, dh as usize);
    assert!(src.len() >= sw * sh * 4 && sw > 0 && sh > 0 && dw > 0 && dh > 0);
    let mut out = vec![0u8; dw * dh * 4];
    let (fx, fy) = (sw as f32 / dw as f32, sh as f32 / dh as f32);
    for y in 0..dh {
        let sy = ((y as f32 + 0.5) * fy - 0.5).clamp(0.0, (sh - 1) as f32);
        let (y0, ty) = (sy.floor() as usize, sy.fract());
        let y1 = (y0 + 1).min(sh - 1);
        for x in 0..dw {
            let sx = ((x as f32 + 0.5) * fx - 0.5).clamp(0.0, (sw - 1) as f32);
            let (x0, tx) = (sx.floor() as usize, sx.fract());
            let x1 = (x0 + 1).min(sw - 1);
            for c in 0..4 {
                let p = |yy: usize, xx: usize| f32::from(src[(yy * sw + xx) * 4 + c]);
                let top = p(y0, x0) * (1.0 - tx) + p(y0, x1) * tx;
                let bot = p(y1, x0) * (1.0 - tx) + p(y1, x1) * tx;
                out[(y * dw + x) * 4 + c] = (top * (1.0 - ty) + bot * ty).round() as u8;
            }
        }
    }
    out
}

/// RGBA8 -> planar CHW f32. With `imagenet` the channels are normalised with the ImageNet
/// mean and std, otherwise they are just scaled to 0..1.
pub fn to_chw(rgba: &[u8], (w, h): (u32, u32), imagenet: bool) -> Vec<f32> {
    let n = w as usize * h as usize;
    assert!(rgba.len() >= n * 4);
    let mut out = vec![0.0f32; n * 3];
    for i in 0..n {
        for c in 0..3 {
            let v = f32::from(rgba[i * 4 + c]) / 255.0;
            out[c * n + i] = if imagenet {
                (v - IMAGENET_MEAN[c]) / IMAGENET_STD[c]
            } else {
                v
            };
        }
    }
    out
}

/// Maps `v` to 0..255 between `lo` and `hi` (flat input gives 0).
pub fn normalize_u8(v: &[f32], lo: f32, hi: f32) -> Vec<u8> {
    let span = hi - lo;
    v.iter()
        .map(|x| {
            if span > 1e-6 && x.is_finite() {
                (((x - lo) / span).clamp(0.0, 1.0) * 255.0).round() as u8
            } else {
                0
            }
        })
        .collect()
}

// ---------------------------------------------------------------- models

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

fn floats(t: &Tensor) -> Result<Vec<f32>, String> {
    Ok(t.to_plain_array_view::<f32>()
        .map_err(err)?
        .iter()
        .copied()
        .collect())
}

fn fact(shape: &[usize]) -> InferenceFact {
    InferenceFact::dt_shape(f32::datum_type(), shape)
}

/// MiDaS small at its fixed 256x256 input.
pub struct Depth {
    plan: Plan,
    size: (u32, u32),
    /// Smoothed min/max of the raw output, so the scale does not flicker frame to frame.
    range: Option<(f32, f32)>,
}

impl Depth {
    pub fn load(path: &Path) -> Result<Self, String> {
        let size = ModelKind::Depth.input_size();
        let plan = tract_onnx::onnx()
            .model_for_path(path)
            .map_err(err)?
            .with_input_fact(0, fact(&[1, 3, size.1 as usize, size.0 as usize]))
            .map_err(err)?
            .into_optimized()
            .map_err(err)?
            .into_runnable()
            .map_err(err)?;
        Ok(Self {
            plan,
            size,
            range: None,
        })
    }

    pub fn reset(&mut self) {
        self.range = None;
    }

    /// `rgba` is any size, resized to the network input when needed.
    pub fn infer(&mut self, rgba: &[u8], src: (u32, u32)) -> Result<Mask, String> {
        let (w, h) = self.size;
        let img = if src == self.size {
            std::borrow::Cow::Borrowed(rgba)
        } else {
            std::borrow::Cow::Owned(resize_rgba(rgba, src, self.size))
        };
        let x = Tensor::from_shape(
            &[1, 3, h as usize, w as usize],
            &to_chw(&img, self.size, true),
        )
        .map_err(err)?;
        let out = self.plan.run(tvec!(x.into())).map_err(err)?;
        let d = floats(&out[0])?;
        let d = d.as_slice();
        if d.len() != (w * h) as usize {
            return Err(format!("unexpected depth output of {} values", d.len()));
        }
        let finite = d.iter().copied().filter(|v| v.is_finite());
        let lo = finite.clone().fold(f32::INFINITY, f32::min);
        let hi = finite.fold(f32::NEG_INFINITY, f32::max);
        let (lo, hi) = if lo.is_finite() && hi.is_finite() {
            match self.range {
                Some((plo, phi)) => (plo + (lo - plo) * 0.3, phi + (hi - phi) * 0.3),
                None => (lo, hi),
            }
        } else {
            (0.0, 0.0)
        };
        self.range = Some((lo, hi));
        Ok(Mask {
            w,
            h,
            data: normalize_u8(d, lo, hi),
        })
    }
}

/// Channels of the four recurrent states of RVM MobileNetV3 and their downscale factors.
const RVM_STATES: [(usize, usize); 4] = [(16, 2), (20, 4), (40, 8), (64, 16)];

/// Robust Video Matting at a fixed size, keeping the recurrent state between frames.
pub struct Matting {
    plan: Plan,
    size: (u32, u32),
    states: Vec<Tensor>,
}

impl Matting {
    pub fn load(path: &Path) -> Result<Self, String> {
        let size = ModelKind::Matting.input_size();
        let (w, h) = (size.0 as usize, size.1 as usize);
        let mut m = tract_onnx::onnx().model_for_path(path).map_err(err)?;
        m = m.with_input_fact(0, fact(&[1, 3, h, w])).map_err(err)?;
        for (i, (c, f)) in RVM_STATES.iter().enumerate() {
            m = m
                .with_input_fact(i + 1, fact(&[1, *c, h / f, w / f]))
                .map_err(err)?;
        }
        let plan = m
            .with_input_fact(
                5,
                InferenceFact::from(Tensor::from_shape(&[1], &[1.0f32]).map_err(err)?),
            )
            .map_err(err)?
            .into_optimized()
            .map_err(err)?
            .into_runnable()
            .map_err(err)?;
        let mut s = Self {
            plan,
            size,
            states: vec![],
        };
        s.reset();
        Ok(s)
    }

    /// Forgets the recurrent state (new clip or source).
    pub fn reset(&mut self) {
        let (w, h) = (self.size.0 as usize, self.size.1 as usize);
        self.states = RVM_STATES
            .iter()
            .map(|(c, f)| Tensor::zero::<f32>(&[1, *c, h / f, w / f]).expect("zero tensor"))
            .collect();
    }

    pub fn infer(&mut self, rgba: &[u8], src: (u32, u32)) -> Result<Mask, String> {
        let pha = self.infer_f32(rgba, src)?;
        Ok(Mask {
            w: self.size.0,
            h: self.size.1,
            data: normalize_u8(&pha, 0.0, 1.0),
        })
    }

    /// Raw alpha matte (0..1 floats, `w * h`), advancing the recurrent state.
    pub fn infer_f32(&mut self, rgba: &[u8], src: (u32, u32)) -> Result<Vec<f32>, String> {
        let (w, h) = self.size;
        let img = if src == self.size {
            std::borrow::Cow::Borrowed(rgba)
        } else {
            std::borrow::Cow::Owned(resize_rgba(rgba, src, self.size))
        };
        let x = Tensor::from_shape(
            &[1, 3, h as usize, w as usize],
            &to_chw(&img, self.size, false),
        )
        .map_err(err)?;
        let ratio = Tensor::from_shape(&[1], &[1.0f32]).map_err(err)?;
        let mut ins: TVec<TValue> = tvec!(x.into());
        ins.extend(self.states.iter().cloned().map(TValue::from));
        ins.push(ratio.into());
        let out = self.plan.run(ins).map_err(err)?;
        if out.len() < 6 {
            return Err(format!("unexpected RVM output count {}", out.len()));
        }
        let pha = floats(&out[1])?;
        if pha.len() != (w * h) as usize {
            return Err(format!("unexpected pha output of {} values", pha.len()));
        }
        for i in 0..4 {
            self.states[i] = out[2 + i].clone().into_tensor();
        }
        Ok(pha)
    }
}

// ---------------------------------------------------------------- worker

/// What a worker is doing.
#[derive(Debug, Clone, PartialEq)]
pub enum Status {
    Loading,
    Ready,
    /// The model file does not exist; carries the user-facing message.
    NotFound(String),
    Failed(String),
}

struct State {
    input: Option<(Vec<u8>, (u32, u32))>,
    output: Option<Mask>,
    reset: bool,
    stop: bool,
    status: Status,
    last_ms: f32,
}

struct Shared {
    st: Mutex<State>,
    cv: Condvar,
}

/// One model instance on its own thread. Dropping it stops the thread after the inference
/// in progress; `Drop` does not wait.
pub struct Worker {
    shared: Arc<Shared>,
    kind: ModelKind,
}

impl Worker {
    pub fn spawn(kind: ModelKind, models_dir: &Path) -> Self {
        let path = kind.path_in(models_dir);
        let missing = !path.is_file();
        let shared = Arc::new(Shared {
            st: Mutex::new(State {
                input: None,
                output: None,
                reset: false,
                stop: false,
                status: if missing {
                    Status::NotFound(kind.missing_message(models_dir))
                } else {
                    Status::Loading
                },
                last_ms: 0.0,
            }),
            cv: Condvar::new(),
        });
        if !missing {
            let sh = shared.clone();
            std::thread::spawn(move || run(kind, &path, &sh));
        }
        Self { shared, kind }
    }

    pub fn kind(&self) -> ModelKind {
        self.kind
    }

    pub fn status(&self) -> Status {
        self.shared.st.lock().unwrap().status.clone()
    }

    /// Milliseconds of the last inference.
    pub fn last_ms(&self) -> f32 {
        self.shared.st.lock().unwrap().last_ms
    }

    /// Queues a frame, replacing one that was not picked up yet. Never waits for inference.
    pub fn submit(&self, rgba: Vec<u8>, size: (u32, u32)) {
        let mut s = self.shared.st.lock().unwrap();
        if matches!(s.status, Status::NotFound(_) | Status::Failed(_)) {
            return;
        }
        s.input = Some((rgba, size));
        self.shared.cv.notify_one();
    }

    /// The newest finished mask, if one arrived since the last call.
    pub fn take(&self) -> Option<Mask> {
        self.shared.st.lock().unwrap().output.take()
    }

    /// Clears recurrent state before the next frame (new clip or source).
    pub fn reset(&self) {
        let mut s = self.shared.st.lock().unwrap();
        s.reset = true;
        s.input = None;
        self.shared.cv.notify_one();
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.shared.st.lock().unwrap().stop = true;
        self.shared.cv.notify_one();
    }
}

enum Net {
    Depth(Depth),
    Matting(Matting),
}

fn run(kind: ModelKind, path: &Path, sh: &Shared) {
    let loaded = std::panic::catch_unwind(|| match kind {
        ModelKind::Depth => Depth::load(path).map(Net::Depth),
        ModelKind::Matting => Matting::load(path).map(Net::Matting),
    })
    .unwrap_or_else(|_| Err("model loading panicked".into()));
    let mut net = match loaded {
        Ok(n) => n,
        Err(e) => {
            sh.st.lock().unwrap().status = Status::Failed(format!("{}: {e}", kind.label()));
            return;
        }
    };
    sh.st.lock().unwrap().status = Status::Ready;
    loop {
        let (frame, reset) = {
            let mut s = sh.st.lock().unwrap();
            while !s.stop && s.input.is_none() && !s.reset {
                s = sh.cv.wait(s).unwrap();
            }
            if s.stop {
                return;
            }
            (s.input.take(), std::mem::take(&mut s.reset))
        };
        if reset {
            match &mut net {
                Net::Depth(d) => d.reset(),
                Net::Matting(m) => m.reset(),
            }
            // A mask from the old source must not outlive the reset.
            sh.st.lock().unwrap().output = None;
        }
        let Some((rgba, size)) = frame else { continue };
        let t0 = Instant::now();
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match &mut net {
            Net::Depth(d) => d.infer(&rgba, size),
            Net::Matting(m) => m.infer(&rgba, size),
        }))
        .unwrap_or_else(|_| Err("inference panicked".into()));
        let mut s = sh.st.lock().unwrap();
        match r {
            Ok(m) => {
                s.last_ms = t0.elapsed().as_secs_f32() * 1000.0;
                if !s.reset {
                    s.output = Some(m);
                }
            }
            Err(e) => {
                s.status = Status::Failed(format!("{}: {e}", kind.label()));
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests;
