//! ISF effects: header parser and GLSL -> WGSL translation through naga.
//!
//! An ISF file is a JSON header in a `/*{ ... }*/` comment followed by a GLSL fragment
//! body. `"PERSISTENT": true` gives the shader its previous output as `PREV_PIXEL`.
//! `"PASSES"` runs the shader several times per frame: a pass with a `TARGET` renders into
//! a named buffer (optionally `PERSISTENT` across frames, sized by `WIDTH`/`HEIGHT`
//! expressions) that the shader reads as an image under that name; the pass index is
//! `PASSINDEX`.
//! Everything here is pure CPU work and returns errors instead of panicking, so the app
//! can run it on a worker thread and disable the effect on failure.

pub mod lut;

use vjc_model::{Effect, Param, Value};

/// Max uniform slots (vec4 each): slot 0 holds TIME and RENDERSIZE, the rest the inputs.
pub const MAX_SLOTS: usize = 16;

/// Uniform block as the compositor uploads it.
pub type Uniforms = [[f32; 4]; MAX_SLOTS];

/// Bundled effects: (id, ISF source).
pub const BUILTIN: &[(&str, &str)] = &[
    ("invert", include_str!("../../../assets/effects/invert.fs")),
    (
        "hue_shift",
        include_str!("../../../assets/effects/hue_shift.fs"),
    ),
    ("mirror", include_str!("../../../assets/effects/mirror.fs")),
    ("tile", include_str!("../../../assets/effects/tile.fs")),
    (
        "kaleido",
        include_str!("../../../assets/effects/kaleido.fs"),
    ),
    (
        "rgb_split",
        include_str!("../../../assets/effects/rgb_split.fs"),
    ),
    (
        "pixelate",
        include_str!("../../../assets/effects/pixelate.fs"),
    ),
    (
        "vignette",
        include_str!("../../../assets/effects/vignette.fs"),
    ),
    ("blur", include_str!("../../../assets/effects/blur.fs")),
    ("depth", include_str!("../../../assets/effects/depth.fs")),
    (
        "remove_bg",
        include_str!("../../../assets/effects/remove_bg.fs"),
    ),
    (
        "brightness_contrast",
        include_str!("../../../assets/effects/brightness_contrast.fs"),
    ),
    ("barrel", include_str!("../../../assets/effects/barrel.fs")),
    ("bulge", include_str!("../../../assets/effects/bulge.fs")),
    (
        "channel_mixer",
        include_str!("../../../assets/effects/channel_mixer.fs"),
    ),
    (
        "chroma_key",
        include_str!("../../../assets/effects/chroma_key.fs"),
    ),
    (
        "chromatic_aberration",
        include_str!("../../../assets/effects/chromatic_aberration.fs"),
    ),
    (
        "color_temperature",
        include_str!("../../../assets/effects/color_temperature.fs"),
    ),
    ("crt", include_str!("../../../assets/effects/crt.fs")),
    (
        "directional_blur",
        include_str!("../../../assets/effects/directional_blur.fs"),
    ),
    (
        "dot_screen",
        include_str!("../../../assets/effects/dot_screen.fs"),
    ),
    (
        "duotone",
        include_str!("../../../assets/effects/duotone.fs"),
    ),
    (
        "edge_detect",
        include_str!("../../../assets/effects/edge_detect.fs"),
    ),
    ("emboss", include_str!("../../../assets/effects/emboss.fs")),
    (
        "film_grain",
        include_str!("../../../assets/effects/film_grain.fs"),
    ),
    (
        "gaussian_blur",
        include_str!("../../../assets/effects/gaussian_blur.fs"),
    ),
    (
        "glitch_slices",
        include_str!("../../../assets/effects/glitch_slices.fs"),
    ),
    ("glow", include_str!("../../../assets/effects/glow.fs")),
    (
        "gradient_map",
        include_str!("../../../assets/effects/gradient_map.fs"),
    ),
    (
        "halftone",
        include_str!("../../../assets/effects/halftone.fs"),
    ),
    ("jitter", include_str!("../../../assets/effects/jitter.fs")),
    ("levels", include_str!("../../../assets/effects/levels.fs")),
    (
        "luma_key",
        include_str!("../../../assets/effects/luma_key.fs"),
    ),
    ("mosaic", include_str!("../../../assets/effects/mosaic.fs")),
    (
        "neon_edges",
        include_str!("../../../assets/effects/neon_edges.fs"),
    ),
    (
        "noise_displace",
        include_str!("../../../assets/effects/noise_displace.fs"),
    ),
    ("polar", include_str!("../../../assets/effects/polar.fs")),
    (
        "posterize",
        include_str!("../../../assets/effects/posterize.fs"),
    ),
    (
        "radial_blur",
        include_str!("../../../assets/effects/radial_blur.fs"),
    ),
    ("ripple", include_str!("../../../assets/effects/ripple.fs")),
    (
        "saturation",
        include_str!("../../../assets/effects/saturation.fs"),
    ),
    (
        "scanlines",
        include_str!("../../../assets/effects/scanlines.fs"),
    ),
    (
        "sharpen",
        include_str!("../../../assets/effects/sharpen.fs"),
    ),
    (
        "solarize",
        include_str!("../../../assets/effects/solarize.fs"),
    ),
    (
        "split_tone",
        include_str!("../../../assets/effects/split_tone.fs"),
    ),
    ("strobe", include_str!("../../../assets/effects/strobe.fs")),
    (
        "threshold",
        include_str!("../../../assets/effects/threshold.fs"),
    ),
    (
        "tilt_shift",
        include_str!("../../../assets/effects/tilt_shift.fs"),
    ),
    ("twirl", include_str!("../../../assets/effects/twirl.fs")),
    (
        "vhs_noise",
        include_str!("../../../assets/effects/vhs_noise.fs"),
    ),
    ("wave", include_str!("../../../assets/effects/wave.fs")),
    ("trails", include_str!("../../../assets/effects/trails.fs")),
    ("echo", include_str!("../../../assets/effects/echo.fs")),
    (
        "blur_separable",
        include_str!("../../../assets/effects/blur_separable.fs"),
    ),
    (
        "feedback_trail",
        include_str!("../../../assets/effects/feedback_trail.fs"),
    ),
    ("crop", include_str!("../../../assets/effects/crop.fs")),
    (
        "keystone",
        include_str!("../../../assets/effects/keystone.fs"),
    ),
    ("freeze", include_str!("../../../assets/effects/freeze.fs")),
    (
        "zoom_blur",
        include_str!("../../../assets/effects/zoom_blur.fs"),
    ),
    (
        "exposure_gamma",
        include_str!("../../../assets/effects/exposure_gamma.fs"),
    ),
    (
        "vibrance",
        include_str!("../../../assets/effects/vibrance.fs"),
    ),
    (
        "color_balance",
        include_str!("../../../assets/effects/color_balance.fs"),
    ),
    ("sepia", include_str!("../../../assets/effects/sepia.fs")),
    (
        "black_white",
        include_str!("../../../assets/effects/black_white.fs"),
    ),
    (
        "colorize",
        include_str!("../../../assets/effects/colorize.fs"),
    ),
    (
        "thermal",
        include_str!("../../../assets/effects/thermal.fs"),
    ),
    (
        "night_vision",
        include_str!("../../../assets/effects/night_vision.fs"),
    ),
    ("sketch", include_str!("../../../assets/effects/sketch.fs")),
    (
        "oil_paint",
        include_str!("../../../assets/effects/oil_paint.fs"),
    ),
    (
        "cartoon",
        include_str!("../../../assets/effects/cartoon.fs"),
    ),
    ("bloom", include_str!("../../../assets/effects/bloom.fs")),
    (
        "fisheye",
        include_str!("../../../assets/effects/fisheye.fs"),
    ),
    ("pinch", include_str!("../../../assets/effects/pinch.fs")),
    ("vortex", include_str!("../../../assets/effects/vortex.fs")),
    ("shake", include_str!("../../../assets/effects/shake.fs")),
    ("flip", include_str!("../../../assets/effects/flip.fs")),
    (
        "rotate_zoom",
        include_str!("../../../assets/effects/rotate_zoom.fs"),
    ),
    (
        "mirror_quad",
        include_str!("../../../assets/effects/mirror_quad.fs"),
    ),
    (
        "infinite_zoom",
        include_str!("../../../assets/effects/infinite_zoom.fs"),
    ),
    (
        "add_subtract",
        include_str!("../../../assets/effects/add_subtract.fs"),
    ),
    (
        "color_pass",
        include_str!("../../../assets/effects/color_pass.fs"),
    ),
    (
        "hue_rotate_cycle",
        include_str!("../../../assets/effects/hue_rotate_cycle.fs"),
    ),
    (
        "contrast_curve",
        include_str!("../../../assets/effects/contrast_curve.fs"),
    ),
    (
        "gamma_rgb",
        include_str!("../../../assets/effects/gamma_rgb.fs"),
    ),
    (
        "exposure_flash",
        include_str!("../../../assets/effects/exposure_flash.fs"),
    ),
    (
        "edge_glow",
        include_str!("../../../assets/effects/edge_glow.fs"),
    ),
    (
        "displace_map",
        include_str!("../../../assets/effects/displace_map.fs"),
    ),
    (
        "wave_warp",
        include_str!("../../../assets/effects/wave_warp.fs"),
    ),
    (
        "stripes_overlay",
        include_str!("../../../assets/effects/stripes_overlay.fs"),
    ),
    ("slide", include_str!("../../../assets/effects/slide.fs")),
    (
        "radial_mosaic",
        include_str!("../../../assets/effects/radial_mosaic.fs"),
    ),
    (
        "stingy_sphere",
        include_str!("../../../assets/effects/stingy_sphere.fs"),
    ),
    ("goo", include_str!("../../../assets/effects/goo.fs")),
    (
        "fragment",
        include_str!("../../../assets/effects/fragment.fs"),
    ),
    (
        "iterate",
        include_str!("../../../assets/effects/iterate.fs"),
    ),
    (
        "shift_glitch",
        include_str!("../../../assets/effects/shift_glitch.fs"),
    ),
    ("lowres", include_str!("../../../assets/effects/lowres.fs")),
    (
        "shadow_drop",
        include_str!("../../../assets/effects/shadow_drop.fs"),
    ),
    (
        "outline",
        include_str!("../../../assets/effects/outline.fs"),
    ),
    (
        "cross_hatch",
        include_str!("../../../assets/effects/cross_hatch.fs"),
    ),
    (
        "rain_drops",
        include_str!("../../../assets/effects/rain_drops.fs"),
    ),
    ("lut3d", include_str!("../../../assets/effects/lut3d.fs")),
    (
        "cloner_grid",
        include_str!("../../../assets/effects/cloner_grid.fs"),
    ),
    (
        "cloner_linear",
        include_str!("../../../assets/effects/cloner_linear.fs"),
    ),
    (
        "cloner_radial",
        include_str!("../../../assets/effects/cloner_radial.fs"),
    ),
    (
        "video_wall",
        include_str!("../../../assets/effects/video_wall.fs"),
    ),
    (
        "strokes",
        include_str!("../../../assets/effects/strokes.fs"),
    ),
];

/// Bundled generators: ISF without an image input, rendered at composition size.
pub const GENERATORS: &[(&str, &str)] = &[
    (
        "solid_color",
        include_str!("../../../assets/generators/solid_color.fs"),
    ),
    (
        "linear_gradient",
        include_str!("../../../assets/generators/linear_gradient.fs"),
    ),
    (
        "radial_gradient",
        include_str!("../../../assets/generators/radial_gradient.fs"),
    ),
    (
        "checker",
        include_str!("../../../assets/generators/checker.fs"),
    ),
    ("noise", include_str!("../../../assets/generators/noise.fs")),
    ("bars", include_str!("../../../assets/generators/bars.fs")),
    (
        "digits",
        include_str!("../../../assets/generators/digits.fs"),
    ),
    (
        "plasma",
        include_str!("../../../assets/generators/plasma.fs"),
    ),
    ("rings", include_str!("../../../assets/generators/rings.fs")),
    (
        "starfield",
        include_str!("../../../assets/generators/starfield.fs"),
    ),
    (
        "voronoi",
        include_str!("../../../assets/generators/voronoi.fs"),
    ),
    ("waves", include_str!("../../../assets/generators/waves.fs")),
    ("text", include_str!("../../../assets/generators/text.fs")),
    (
        "tunnel",
        include_str!("../../../assets/generators/tunnel.fs"),
    ),
    (
        "metaballs",
        include_str!("../../../assets/generators/metaballs.fs"),
    ),
    ("fire", include_str!("../../../assets/generators/fire.fs")),
    (
        "clouds",
        include_str!("../../../assets/generators/clouds.fs"),
    ),
    ("lines", include_str!("../../../assets/generators/lines.fs")),
    (
        "circles_grid",
        include_str!("../../../assets/generators/circles_grid.fs"),
    ),
    (
        "triangles",
        include_str!("../../../assets/generators/triangles.fs"),
    ),
    (
        "lens_flare",
        include_str!("../../../assets/generators/lens_flare.fs"),
    ),
    (
        "particles",
        include_str!("../../../assets/generators/particles.fs"),
    ),
    (
        "gradient_sweep",
        include_str!("../../../assets/generators/gradient_sweep.fs"),
    ),
    (
        "spectrum_bars",
        include_str!("../../../assets/generators/spectrum_bars.fs"),
    ),
    (
        "waveform_line",
        include_str!("../../../assets/generators/waveform_line.fs"),
    ),
    (
        "grid_3d",
        include_str!("../../../assets/generators/grid_3d.fs"),
    ),
    (
        "hex_grid",
        include_str!("../../../assets/generators/hex_grid.fs"),
    ),
    (
        "spiral",
        include_str!("../../../assets/generators/spiral.fs"),
    ),
    (
        "lissajous",
        include_str!("../../../assets/generators/lissajous.fs"),
    ),
    (
        "stars_warp",
        include_str!("../../../assets/generators/stars_warp.fs"),
    ),
    (
        "lava_lamp",
        include_str!("../../../assets/generators/lava_lamp.fs"),
    ),
    (
        "text_block",
        include_str!("../../../assets/generators/text_block.fs"),
    ),
    (
        "text_animator",
        include_str!("../../../assets/generators/text_animator.fs"),
    ),
    (
        "test_card",
        include_str!("../../../assets/generators/test_card.fs"),
    ),
    (
        "abstract_field",
        include_str!("../../../assets/generators/abstract_field.fs"),
    ),
    (
        "stroboscope",
        include_str!("../../../assets/generators/stroboscope.fs"),
    ),
    (
        "shaper",
        include_str!("../../../assets/generators/shaper.fs"),
    ),
];

/// Source of a bundled effect or generator.
pub fn builtin_source(id: &str) -> Option<&'static str> {
    BUILTIN
        .iter()
        .chain(GENERATORS)
        .find(|(k, _)| *k == id)
        .map(|(_, s)| *s)
}

/// True when the bundled shader `id` declares an ISF `audio` / `audioFFT` input.
pub fn uses_audio(id: &str) -> bool {
    static KINDS: std::sync::OnceLock<Vec<&'static str>> = std::sync::OnceLock::new();
    KINDS
        .get_or_init(|| {
            BUILTIN
                .iter()
                .chain(GENERATORS)
                .filter(|(_, src)| parse(src).is_ok_and(|i| !i.audio.is_empty()))
                .map(|(k, _)| *k)
                .collect()
        })
        .contains(&id)
}

/// Default params of a bundled generator (None if `id` is not one).
pub fn generator_params(id: &str) -> Option<Vec<Param>> {
    let (_, src) = GENERATORS.iter().find(|(k, _)| *k == id)?;
    parse(src).ok().map(|i| i.default_params())
}

#[derive(Debug, Clone, PartialEq)]
pub struct FxError(pub String);

impl std::fmt::Display for FxError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for FxError {}

fn err<T>(s: impl Into<String>) -> Result<T, FxError> {
    Err(FxError(s.into()))
}

#[derive(Debug, Clone, PartialEq)]
pub enum InputKind {
    Float {
        min: f64,
        max: f64,
        default: f64,
    },
    Bool {
        default: bool,
    },
    Color {
        default: [f32; 4],
    },
    Point2D {
        min: [f64; 2],
        max: [f64; 2],
        default: [f64; 2],
    },
    Long {
        values: Vec<i64>,
        labels: Vec<String>,
        default: usize,
    },
    /// String input: `TEXT_CHARS` character codes packed into `TEXT_SLOTS` uniform slots.
    /// The shader reads them through `isf_u.p[<name> + i / 4][i % 4]`.
    Text {
        default: String,
    },
}

/// Characters a text input carries.
pub const TEXT_CHARS: usize = 24;
/// Uniform slots a text input takes (4 character codes per slot).
pub const TEXT_SLOTS: usize = TEXT_CHARS / 4;

impl InputKind {
    /// Uniform slots this input takes.
    pub fn slots(&self) -> usize {
        match self {
            InputKind::Text { .. } => TEXT_SLOTS,
            _ => 1,
        }
    }
}

/// Character codes of `s` as the shader reads them: ASCII only, padded with spaces,
/// anything else becomes a space.
pub fn text_codes(s: &str) -> [f32; TEXT_CHARS] {
    let mut out = [32.0; TEXT_CHARS];
    for (o, c) in out.iter_mut().zip(s.chars()) {
        *o = if c.is_ascii() && !c.is_ascii_control() {
            c as u32 as f32
        } else {
            32.0
        };
    }
    out
}

#[derive(Debug, Clone, PartialEq)]
pub struct Input {
    pub name: String,
    pub kind: InputKind,
    /// Optional `"GROUP"` of the ISF input: the fold section it is shown in.
    pub group: Option<String>,
}

/// Sections of the parameter panel as (title, input indices); `None` is the loose block
/// shown first. Explicit `GROUP` wins; otherwise inputs that share a prefix before the
/// first `_` (`blur_amount`, `blur_angle`) form a group named after it. A prefix used by a
/// single input does not make a section. With no group at all the result is one loose block.
pub fn param_groups(inputs: &[Input]) -> Vec<(Option<String>, Vec<usize>)> {
    let prefix = |n: &str| {
        n.split_once('_')
            .map(|(p, _)| p)
            .filter(|p| !p.is_empty())
            .map(str::to_string)
    };
    let shared = |p: &str| {
        inputs
            .iter()
            .filter(|i| prefix(&i.name).as_deref() == Some(p))
            .count()
            > 1
    };
    let title = |s: &str| {
        let mut c = s.chars();
        c.next()
            .map(|f| f.to_uppercase().chain(c).collect::<String>())
            .unwrap_or_default()
    };
    let mut out: Vec<(Option<String>, Vec<usize>)> = vec![(None, vec![])];
    for (i, inp) in inputs.iter().enumerate() {
        let g = inp
            .group
            .clone()
            .filter(|g| !g.trim().is_empty())
            .or_else(|| prefix(&inp.name).filter(|p| shared(p)).map(|p| title(&p)));
        match g {
            None => out[0].1.push(i),
            Some(g) => match out
                .iter_mut()
                .find(|(t, _)| t.as_deref() == Some(g.as_str()))
            {
                Some((_, v)) => v.push(i),
                None => out.push((Some(g), vec![i])),
            },
        }
    }
    if out[0].1.is_empty() && out.len() > 1 {
        out.remove(0);
    }
    out
}

/// An ISF `"TYPE": "audio"` (waveform) or `"audioFFT"` (spectrum) input: a one row texture
/// the shader samples with `IMG_NORM_PIXEL(name, vec2(x, 0.5))`. Channel r holds the value
/// (0..1); channel a is 1 while audio is playing and 0 otherwise, so a shader can fall back
/// to a synthetic picture when it reads 0 at `x = 0.5`.
#[derive(Debug, Clone, PartialEq)]
pub struct AudioInput {
    pub name: String,
    /// `audioFFT`: spectrum bins, low to high. Otherwise the waveform.
    pub fft: bool,
    /// ISF `MAX`: samples the shader asks for (1..=`AUDIO_TEX_W`, default 64). The runtime
    /// texture is always `AUDIO_TEX_W` wide, so this only tells the shader's author what
    /// resolution is meaningful.
    pub samples: usize,
}

/// Width of the audio textures, in texels.
pub const AUDIO_TEX_W: usize = 256;
/// Bind group slot of the spectrum texture; the waveform is the next one.
pub const AUDIO_BINDING: u32 = TARGET_BINDING + MAX_TARGETS as u32;

/// RGBA8 row of `AUDIO_TEX_W` texels for an audio texture: `data` (0..1) stretched over the
/// row, alpha 255 when `active`. Without audio the row is all zero.
pub fn audio_texels(data: &[f32], active: bool) -> Vec<u8> {
    let mut out = vec![0u8; AUDIO_TEX_W * 4];
    if !active || data.is_empty() {
        return out;
    }
    for (k, px) in out.as_chunks_mut::<4>().0.iter_mut().enumerate() {
        let src = (k * data.len() / AUDIO_TEX_W).min(data.len() - 1);
        let v = data[src];
        let v = if v.is_finite() {
            v.clamp(0.0, 1.0)
        } else {
            0.0
        };
        *px = [(v * 255.0).round() as u8, 0, 0, 255];
    }
    out
}

#[derive(Debug, Clone, PartialEq)]
pub struct Isf {
    /// Audio inputs (not listed in `inputs`: they are textures, not uniforms).
    pub audio: Vec<AudioInput>,
    pub description: String,
    pub categories: Vec<String>,
    /// Controllable inputs, in file order (the image input is not listed).
    pub inputs: Vec<Input>,
    /// The effect keeps a per-instance buffer with its previous output (`PREV_PIXEL`):
    /// trails, echo, feedback. Header: `"PERSISTENT": true`.
    pub persistent: bool,
    /// Name of the float input that is the capture rate in ticks per second, for a
    /// persistent effect that grabs a new frame per tick (freeze). Header:
    /// `"TICK_INPUT": "fps"`. The runtime then tells the shader, per instance, whether
    /// this frame starts a new tick (`NEW_TICK`).
    pub tick_input: Option<String>,
    /// ISF `PASSES`, in order (empty = one ordinary pass).
    pub passes: Vec<Pass>,
    /// GLSL after the header.
    pub body: String,
}

/// Named buffers a multipass effect may declare (bindings `TARGET_BINDING..`).
pub const MAX_TARGETS: usize = 4;
/// First bind group slot of the named targets.
pub const TARGET_BINDING: u32 = 5;
/// Most passes one effect may run per frame.
pub const MAX_PASSES: usize = 8;
/// Largest target side in pixels.
pub const MAX_TARGET_SIDE: u32 = 8192;

/// One entry of the ISF `PASSES` array.
#[derive(Debug, Clone, PartialEq)]
pub struct Pass {
    /// Named buffer the pass renders into (None = the effect output).
    pub target: Option<String>,
    /// The target keeps its content across frames (needs a `target`).
    pub persistent: bool,
    /// ISF asks for a float buffer. Buffers are 8-bit here, so this is only recorded.
    pub float: bool,
    /// Size expressions (`$WIDTH`, `$HEIGHT`, numbers, `+ - * /`, parentheses).
    pub width: Option<String>,
    pub height: Option<String>,
}

impl Pass {
    /// Pixel size of this pass's buffer for a render size.
    pub fn size(&self, render: (u32, u32)) -> (u32, u32) {
        let side = |e: &Option<String>, r: u32| match e {
            None => r,
            Some(e) => eval_size(e, render)
                .map(|v| v.round().clamp(1.0, MAX_TARGET_SIDE as f32) as u32)
                .unwrap_or(r),
        };
        (side(&self.width, render.0), side(&self.height, render.1))
    }
}

/// Evaluates a pass size expression for a render size. Variables: `$WIDTH`, `$HEIGHT`.
pub fn eval_size(expr: &str, (w, h): (u32, u32)) -> Result<f32, String> {
    struct P<'a> {
        c: Vec<char>,
        i: usize,
        vars: (f32, f32),
        src: &'a str,
    }
    impl P<'_> {
        fn ws(&mut self) {
            while self.c.get(self.i).is_some_and(|c| c.is_whitespace()) {
                self.i += 1;
            }
        }
        fn expr(&mut self) -> Result<f32, String> {
            let mut v = self.term()?;
            loop {
                self.ws();
                match self.c.get(self.i) {
                    Some('+') => {
                        self.i += 1;
                        v += self.term()?;
                    }
                    Some('-') => {
                        self.i += 1;
                        v -= self.term()?;
                    }
                    _ => return Ok(v),
                }
            }
        }
        fn term(&mut self) -> Result<f32, String> {
            let mut v = self.atom()?;
            loop {
                self.ws();
                match self.c.get(self.i) {
                    Some('*') => {
                        self.i += 1;
                        v *= self.atom()?;
                    }
                    Some('/') => {
                        self.i += 1;
                        let d = self.atom()?;
                        if d == 0.0 {
                            return Err(format!("division by zero in size \"{}\"", self.src));
                        }
                        v /= d;
                    }
                    _ => return Ok(v),
                }
            }
        }
        fn atom(&mut self) -> Result<f32, String> {
            self.ws();
            let bad = |s: &str| format!("bad size expression \"{s}\"");
            match self.c.get(self.i).copied() {
                Some('(') => {
                    self.i += 1;
                    let v = self.expr()?;
                    self.ws();
                    if self.c.get(self.i) != Some(&')') {
                        return Err(bad(self.src));
                    }
                    self.i += 1;
                    Ok(v)
                }
                Some('-') => {
                    self.i += 1;
                    Ok(-self.atom()?)
                }
                Some('$') => {
                    let st = self.i + 1;
                    self.i = st;
                    while self.c.get(self.i).is_some_and(|c| c.is_ascii_alphabetic()) {
                        self.i += 1;
                    }
                    match self.c[st..self.i].iter().collect::<String>().as_str() {
                        "WIDTH" => Ok(self.vars.0),
                        "HEIGHT" => Ok(self.vars.1),
                        other => Err(format!(
                            "unknown variable ${other} in size \"{}\" (only $WIDTH and $HEIGHT)",
                            self.src
                        )),
                    }
                }
                Some(c) if c.is_ascii_digit() || c == '.' => {
                    let st = self.i;
                    while self
                        .c
                        .get(self.i)
                        .is_some_and(|c| c.is_ascii_digit() || *c == '.')
                    {
                        self.i += 1;
                    }
                    let t: String = self.c[st..self.i].iter().collect();
                    t.parse().map_err(|_| bad(self.src))
                }
                _ => Err(bad(self.src)),
            }
        }
    }
    let mut p = P {
        c: expr.chars().collect(),
        i: 0,
        vars: (w as f32, h as f32),
        src: expr,
    };
    let v = p.expr()?;
    p.ws();
    if p.i != p.c.len() || !v.is_finite() {
        return Err(format!("bad size expression \"{expr}\""));
    }
    Ok(v)
}

/// Distinct `TARGET` names of `passes` in first-use order.
pub fn targets_of(passes: &[Pass]) -> Vec<&str> {
    let mut v: Vec<&str> = vec![];
    for t in passes.iter().filter_map(|p| p.target.as_deref()) {
        if !v.contains(&t) {
            v.push(t);
        }
    }
    v
}

fn valid_ident(name: &str) -> bool {
    !name.is_empty()
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        && !name.starts_with(|c: char| c.is_ascii_digit())
        && !RESERVED.contains(&name)
        && !name.starts_with("isf_")
        && !name.starts_with("vjc_")
        && !name.starts_with("gl_")
}

fn parse_passes(json: &serde_json::Value, inputs: &[Input]) -> Result<Vec<Pass>, FxError> {
    let Some(list) = json.get("PASSES") else {
        return Ok(vec![]);
    };
    let Some(list) = list.as_array() else {
        return err("PASSES must be an array");
    };
    if list.is_empty() {
        return err("PASSES is empty");
    }
    if list.len() > MAX_PASSES {
        return err(format!("too many PASSES ({} > {MAX_PASSES})", list.len()));
    }
    let mut out = vec![];
    let mut targets: Vec<&str> = vec![];
    for (n, p) in list.iter().enumerate() {
        let Some(o) = p.as_object() else {
            return err(format!("pass {n} is not an object"));
        };
        let target = match o.get("TARGET") {
            None => None,
            Some(serde_json::Value::String(t)) => {
                if !valid_ident(t) || inputs.iter().any(|i| i.name == *t) || t == "inputImage" {
                    return err(format!("pass {n}: invalid TARGET name \"{t}\""));
                }
                if !targets.contains(&t.as_str()) {
                    targets.push(t);
                }
                Some(t.clone())
            }
            Some(_) => return err(format!("pass {n}: TARGET must be a string")),
        };
        let flag = |k: &str| match o.get(k) {
            None => Ok(false),
            Some(serde_json::Value::Bool(b)) => Ok(*b),
            Some(v) => v
                .as_f64()
                .map(|x| x != 0.0)
                .ok_or_else(|| FxError(format!("pass {n}: {k} must be a boolean"))),
        };
        let persistent = flag("PERSISTENT")?;
        let float = flag("FLOAT")?;
        if persistent && target.is_none() {
            return err(format!("pass {n}: PERSISTENT needs a TARGET"));
        }
        let size = |k: &str| -> Result<Option<String>, FxError> {
            let e = match o.get(k) {
                None => return Ok(None),
                Some(serde_json::Value::String(s)) => s.clone(),
                Some(v) if v.is_number() => v.to_string(),
                Some(_) => return err(format!("pass {n}: {k} must be an expression")),
            };
            match eval_size(&e, (100, 100)) {
                Ok(v) if v >= 0.5 => Ok(Some(e)),
                Ok(_) => err(format!("pass {n}: {k} \"{e}\" is not a positive size")),
                Err(m) => err(format!("pass {n}: {k}: {m}")),
            }
        };
        out.push(Pass {
            target,
            persistent,
            float,
            width: size("WIDTH")?,
            height: size("HEIGHT")?,
        });
    }
    if out.last().is_some_and(|p| p.target.is_some()) {
        return err("the last pass renders the effect output and cannot have a TARGET");
    }
    if targets.len() > MAX_TARGETS {
        return err(format!(
            "too many TARGET buffers ({} > {MAX_TARGETS})",
            targets.len()
        ));
    }
    Ok(out)
}

fn nums(v: Option<&serde_json::Value>) -> Option<Vec<f64>> {
    v?.as_array()?.iter().map(|x| x.as_f64()).collect()
}

fn pair(v: Option<&serde_json::Value>, fallback: [f64; 2]) -> [f64; 2] {
    match nums(v).as_deref() {
        Some([a, b, ..]) => [*a, *b],
        _ => fallback,
    }
}

/// Parses an ISF file (header + body).
pub fn parse(src: &str) -> Result<Isf, FxError> {
    let start = src.find("/*").ok_or(FxError("missing ISF header".into()))?;
    let end = src[start..]
        .find("*/")
        .map(|i| i + start)
        .ok_or(FxError("unterminated ISF header".into()))?;
    let json: serde_json::Value = serde_json::from_str(&src[start + 2..end])
        .map_err(|e| FxError(format!("invalid ISF header: {e}")))?;
    let str_of = |k: &str| {
        json.get(k)
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string()
    };
    let categories = json
        .get("CATEGORIES")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    let mut inputs: Vec<Input> = vec![];
    let mut audio: Vec<AudioInput> = vec![];
    for i in json
        .get("INPUTS")
        .and_then(|v| v.as_array())
        .map(Vec::as_slice)
        .unwrap_or_default()
    {
        let name = i
            .get("NAME")
            .and_then(|v| v.as_str())
            .ok_or(FxError("input without NAME".into()))?
            .to_string();
        let ty = i.get("TYPE").and_then(|v| v.as_str()).unwrap_or("");
        let f = |k: &str, d: f64| i.get(k).and_then(|v| v.as_f64()).unwrap_or(d);
        let kind = match ty {
            "image" => continue,
            "audio" | "audioFFT" => {
                if !valid_ident(&name) || inputs.iter().any(|i| i.name == name) {
                    return err(format!("invalid input name {name}"));
                }
                if audio.iter().any(|a: &AudioInput| a.name == name) {
                    return err(format!("duplicate input {name}"));
                }
                audio.push(AudioInput {
                    name,
                    fft: ty == "audioFFT",
                    samples: (f("MAX", 64.0) as usize).clamp(1, AUDIO_TEX_W),
                });
                continue;
            }
            "float" => {
                let (min, max) = (f("MIN", 0.0), f("MAX", 1.0));
                InputKind::Float {
                    min,
                    max,
                    default: f("DEFAULT", min).clamp(min.min(max), max.max(min)),
                }
            }
            "bool" => InputKind::Bool {
                default: match i.get("DEFAULT") {
                    Some(serde_json::Value::Bool(b)) => *b,
                    Some(v) => v.as_f64().unwrap_or(0.0) != 0.0,
                    None => false,
                },
            },
            "color" => {
                let d = nums(i.get("DEFAULT")).unwrap_or_default();
                let g = |k: usize, def: f32| d.get(k).map_or(def, |x| *x as f32);
                InputKind::Color {
                    default: [g(0, 1.0), g(1, 1.0), g(2, 1.0), g(3, 1.0)],
                }
            }
            "point2D" => InputKind::Point2D {
                min: pair(i.get("MIN"), [0.0, 0.0]),
                max: pair(i.get("MAX"), [1.0, 1.0]),
                default: pair(i.get("DEFAULT"), [0.0, 0.0]),
            },
            "long" => {
                let values: Vec<i64> = nums(i.get("VALUES"))
                    .map(|v| v.iter().map(|x| *x as i64).collect())
                    .unwrap_or_default();
                if values.is_empty() {
                    return err(format!("long input {name} without VALUES"));
                }
                let labels = i
                    .get("LABELS")
                    .and_then(|v| v.as_array())
                    .map(|a| {
                        a.iter()
                            .map(|x| x.as_str().unwrap_or("").to_string())
                            .collect()
                    })
                    .unwrap_or_else(|| values.iter().map(|v| v.to_string()).collect());
                let def = i
                    .get("DEFAULT")
                    .and_then(|v| v.as_i64())
                    .unwrap_or(values[0]);
                let default = values.iter().position(|v| *v == def).unwrap_or(0);
                InputKind::Long {
                    values,
                    labels,
                    default,
                }
            }
            "text" => InputKind::Text {
                default: i
                    .get("DEFAULT")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .chars()
                    .take(TEXT_CHARS)
                    .collect(),
            },
            other => return err(format!("unsupported input type {other} ({name})")),
        };
        if !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            || name.starts_with(|c: char| c.is_ascii_digit())
            || RESERVED.contains(&name.as_str())
            || name.starts_with("isf_")
            || name.starts_with("vjc_u_")
            || name.starts_with("gl_")
        {
            return err(format!("invalid input name {name}"));
        }
        let group = i
            .get("GROUP")
            .and_then(|v| v.as_str())
            .map(|g| g.trim().chars().take(40).collect::<String>())
            .filter(|g| !g.is_empty());
        inputs.push(Input { name, kind, group });
    }
    if inputs.iter().map(|i| i.kind.slots()).sum::<usize>() >= MAX_SLOTS {
        return err("too many inputs");
    }
    let persistent = json
        .get("PERSISTENT")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let tick_input = json
        .get("TICK_INPUT")
        .and_then(|v| v.as_str())
        .filter(|n| {
            inputs
                .iter()
                .any(|i| i.name == *n && matches!(i.kind, InputKind::Float { .. }))
        })
        .map(str::to_string);
    let passes = parse_passes(&json, &inputs)?;
    if !passes.is_empty() && persistent {
        return err("PERSISTENT with PASSES: mark the pass TARGET persistent instead");
    }
    if !passes.is_empty() && tick_input.is_some() {
        return err("TICK_INPUT cannot be combined with PASSES");
    }
    Ok(Isf {
        audio,
        persistent,
        tick_input,
        passes,
        description: str_of("DESCRIPTION"),
        categories,
        inputs,
        body: src[end + 2..].to_string(),
    })
}

/// Names an input must not take. Inputs are declared as `#define vjc_u_<name> expr` and
/// the shader body is rewritten (see `rename_inputs`), so ordinary function names such as
/// `mix` or `length` are fine as input names. Only ISF builtins, names the generator
/// declares, and GLSL keywords/types (which cannot be variable names) are rejected.
const RESERVED: &[&str] = &[
    "TIME",
    "NEW_TICK",
    "RENDERSIZE",
    "PASSINDEX",
    "FRAMEINDEX",
    "TIMEDELTA",
    "DATE",
    "IMG_PIXEL",
    "IMG_NORM_PIXEL",
    "IMG_THIS_PIXEL",
    "IMG_SIZE",
    "PREV_PIXEL",
    "PREV_THIS_PIXEL",
    "inputImage",
    "main",
    "void",
    "float",
    "int",
    "uint",
    "bool",
    "vec2",
    "vec3",
    "vec4",
    "ivec2",
    "ivec3",
    "ivec4",
    "mat2",
    "mat3",
    "mat4",
    "sampler2D",
    "sampler",
    "uniform",
    "layout",
    "in",
    "out",
    "inout",
    "if",
    "else",
    "for",
    "while",
    "do",
    "return",
    "const",
    "struct",
    "discard",
    "break",
    "continue",
    "true",
    "false",
    "switch",
    "case",
    "default",
    "precision",
    "highp",
    "mediump",
    "lowp",
    "bvec2",
    "bvec3",
    "bvec4",
    "uvec2",
    "uvec3",
    "uvec4",
    "mat2x2",
    "mat2x3",
    "mat2x4",
    "mat3x2",
    "mat3x3",
    "mat3x4",
    "mat4x2",
    "mat4x3",
    "mat4x4",
    "double",
    "attribute",
    "varying",
    "buffer",
    "shared",
    "centroid",
    "flat",
    "smooth",
    "invariant",
    "sampler3D",
    "samplerCube",
];

const TYPES: &[&str] = &[
    "float", "int", "uint", "bool", "vec2", "vec3", "vec4", "ivec2", "ivec3", "ivec4", "uvec2",
    "uvec3", "uvec4", "bvec2", "bvec3", "bvec4", "mat2", "mat3", "mat4",
];

/// First input name that the body declares as a variable, parameter or struct member
/// (`float name`). The rewrite to `vjc_u_<name>` would turn that into invalid GLSL, so
/// the effect is rejected with a clear message instead. Comments are skipped.
fn declared_input(body: &str, names: &[&str]) -> Option<String> {
    let c: Vec<char> = body.chars().collect();
    // User struct types declare variables like the built-in ones (`Light amount;`).
    let mut structs: Vec<String> = vec![];
    let mut after_struct = false;
    let mut i = 0;
    let mut prev_type = false;
    // Nesting depth, and the depth of the declaration whose declarator list is open
    // (`float a, amount;`: the comma at that depth starts another declarator).
    let mut depth = 0i32;
    let mut decl: Option<i32> = None;
    while i < c.len() {
        if c[i] == '/' && c.get(i + 1) == Some(&'/') {
            while i < c.len() && c[i] != '\n' {
                i += 1;
            }
        } else if c[i] == '/' && c.get(i + 1) == Some(&'*') {
            i += 2;
            while i < c.len() && !(c[i] == '*' && c.get(i + 1) == Some(&'/')) {
                i += 1;
            }
            i += 2;
        } else if c[i].is_ascii_alphanumeric() || c[i] == '_' {
            let st = i;
            while i < c.len() && (c[i].is_ascii_alphanumeric() || c[i] == '_') {
                i += 1;
            }
            let id: String = c[st..i].iter().collect();
            if after_struct {
                structs.push(id.clone());
            }
            after_struct = id == "struct";
            if prev_type && names.contains(&id.as_str()) {
                return Some(id);
            }
            if prev_type {
                decl = Some(depth);
            }
            prev_type = TYPES.contains(&id.as_str()) || structs.contains(&id);
        } else {
            let ch = c[i];
            if !ch.is_whitespace() {
                prev_type = false;
            }
            match ch {
                '(' | '[' | '{' => depth += 1,
                ')' | ']' | '}' => {
                    depth -= 1;
                    if decl.is_some_and(|d| depth < d) {
                        decl = None;
                    }
                }
                ';' => decl = None,
                ',' if decl == Some(depth) => prev_type = true,
                _ => {}
            }
            i += 1;
        }
    }
    None
}

/// Rewrites whole-identifier uses of each input name in `body` to `vjc_u_<name>`.
/// Skips comments, swizzles/members (`.name`) and calls (`name(`), so a float input
/// called `mix` leaves the `mix()` builtin alone.
fn rename_inputs(body: &str, names: &[&str]) -> String {
    let c: Vec<char> = body.chars().collect();
    let mut out = String::with_capacity(body.len() + 16);
    let mut i = 0;
    let mut prev = ' ';
    while i < c.len() {
        let ch = c[i];
        if ch == '/' && c.get(i + 1) == Some(&'/') {
            while i < c.len() && c[i] != '\n' {
                out.push(c[i]);
                i += 1;
            }
        } else if ch == '/' && c.get(i + 1) == Some(&'*') {
            let mut j = i + 2;
            while j < c.len() && !(c[j] == '*' && c.get(j + 1) == Some(&'/')) {
                j += 1;
            }
            let j = (j + 2).min(c.len());
            out.extend(&c[i..j]);
            i = j;
        } else if ch.is_ascii_digit() {
            while i < c.len() && (c[i].is_ascii_alphanumeric() || c[i] == '_') {
                out.push(c[i]);
                i += 1;
            }
            prev = '0';
        } else if ch.is_ascii_alphabetic() || ch == '_' {
            let st = i;
            while i < c.len() && (c[i].is_ascii_alphanumeric() || c[i] == '_') {
                i += 1;
            }
            let id: String = c[st..i].iter().collect();
            let next = c[i..].iter().find(|x| !x.is_whitespace());
            if names.contains(&id.as_str()) && prev != '.' && next != Some(&'(') {
                out.push_str("vjc_u_");
            }
            out.push_str(&id);
            prev = 'a';
        } else {
            out.push(ch);
            if !ch.is_whitespace() {
                prev = ch;
            }
            i += 1;
        }
    }
    out
}

/// Finite floats only: NaN/inf in a uniform turns the whole frame black.
fn finite(x: f32) -> f32 {
    if x.is_finite() { x } else { 0.0 }
}

impl Isf {
    /// Default parameters, one per input (same order).
    pub fn default_params(&self) -> Vec<Param> {
        self.inputs
            .iter()
            .map(|i| {
                let (value, range) = match &i.kind {
                    InputKind::Float { min, max, default } => {
                        (Value::Number(*default), Some((*min, *max)))
                    }
                    InputKind::Bool { default } => (Value::Bool(*default), None),
                    InputKind::Color { default } => (Value::Color(*default), None),
                    InputKind::Point2D { default, min, max } => (
                        Value::Color([default[0] as f32, default[1] as f32, 0.0, 1.0]),
                        Some((min[0].min(min[1]), max[0].max(max[1]))),
                    ),
                    InputKind::Text { default } => (Value::Text(default.clone()), None),
                    InputKind::Long {
                        labels, default, ..
                    } => (
                        Value::Choice {
                            selected: *default as u32,
                            options: labels.clone(),
                        },
                        None,
                    ),
                };
                Param {
                    name: i.name.clone(),
                    default: value.clone(),
                    value,
                    range,
                    animation: None,
                }
            })
            .collect()
    }

    /// Fills the uniform block: slot 0 = (TIME, width, height, tick rate), then one slot per
    /// input. The tick rate (0 when the effect has no `TICK_INPUT`) is read by the runtime,
    /// which replaces it with the `NEW_TICK` flag (see `tick_flag`) before the draw.
    /// Params are matched by name; missing or mistyped ones fall back to the default.
    pub fn pack(&self, params: &[Param], time: f32, size: (u32, u32)) -> Uniforms {
        let mut u = [[0.0; 4]; MAX_SLOTS];
        u[0] = [time, size.0 as f32, size.1 as f32, 0.0];
        let defaults = self.default_params();
        if let Some(name) = &self.tick_input
            && let Some(Value::Number(r)) = params
                .iter()
                .find(|p| p.name == *name)
                .map(|p| &p.value)
                .or_else(|| defaults.iter().find(|p| p.name == *name).map(|p| &p.value))
            && r.is_finite()
            && *r > 0.0
        {
            u[0][3] = *r as f32;
        }
        for ((inp, def), n) in self.inputs.iter().zip(&defaults).zip(self.first_slots()) {
            let v = params
                .iter()
                .find(|p| p.name == inp.name)
                .map_or(&def.value, |p| &p.value);
            if let (InputKind::Text { .. }, Value::Text(t)) = (&inp.kind, v) {
                for (k, c) in text_codes(t).chunks(4).enumerate() {
                    u[n + k] = [c[0], c[1], c[2], c[3]];
                }
                continue;
            }
            if let InputKind::Text { default } = &inp.kind {
                for (k, c) in text_codes(default).chunks(4).enumerate() {
                    u[n + k] = [c[0], c[1], c[2], c[3]];
                }
                continue;
            }
            u[n] = match (&inp.kind, v) {
                (InputKind::Float { .. }, Value::Number(x)) => [*x as f32, 0.0, 0.0, 0.0],
                (InputKind::Bool { .. }, Value::Bool(b)) => [*b as u8 as f32, 0.0, 0.0, 0.0],
                (InputKind::Color { .. }, Value::Color(c)) => *c,
                (InputKind::Point2D { .. }, Value::Color(c)) => [c[0], c[1], 0.0, 0.0],
                (InputKind::Long { values, .. }, Value::Choice { selected, .. }) => {
                    let i = (*selected as usize).min(values.len() - 1);
                    [values[i] as f32, 0.0, 0.0, 0.0]
                }
                _ => pack_default(&def.value),
            }
            .map(finite);
        }
        u
    }
}

/// Value of `NEW_TICK` for one draw of a ticking instance: `time * rate` floored is the
/// tick; `last` is the tick of the previous draw of the same instance (None = it has no
/// history yet). 2.0 = first draw, grab whatever the state; 1.0 = the tick changed, grab
/// unless held; 0.0 = same tick, keep. Any change counts, however many ticks passed (time
/// can skip an even number), and a clock that went back counts too.
pub fn tick_flag(time: f32, rate: f32, last: Option<i64>) -> (f32, i64) {
    let tick = (f64::from(time) * f64::from(rate)).floor() as i64;
    let flag = match last {
        None => 2.0,
        Some(l) if l != tick => 1.0,
        Some(_) => 0.0,
    };
    (flag, tick)
}

fn pack_default(v: &Value) -> [f32; 4] {
    match v {
        Value::Number(x) => [*x as f32, 0.0, 0.0, 0.0],
        Value::Bool(b) => [*b as u8 as f32, 0.0, 0.0, 0.0],
        Value::Color(c) => *c,
        _ => [0.0; 4],
    }
}

/// New effect instance with default params; `kind` is a bundled id.
pub fn new_effect(kind: &str) -> Option<Effect> {
    let isf = parse(builtin_source(kind)?).ok()?;
    Some(Effect {
        blend: Default::default(),
        id: uuid::Uuid::new_v4(),
        kind: kind.to_string(),
        bypass: false,
        dry_wet: 1.0,
        params: isf.default_params(),
    })
}

/// GLSL that naga reads: ISF preamble + body. Each input becomes a macro onto a uniform slot.
pub fn glsl_source(isf: &Isf) -> String {
    let mut s = String::from(
        "#version 450\n\
         layout(location = 0) in vec2 isf_FragNormCoord;\n\
         layout(location = 0) out vec4 isf_FragColor;\n\
         layout(std140, set = 0, binding = 0) uniform ISFU { vec4 p[16]; } isf_u;\n\
         layout(set = 0, binding = 1) uniform texture2D inputImage;\n\
         layout(set = 0, binding = 2) uniform sampler isf_smp;\n\
         layout(set = 0, binding = 3) uniform texture2D vjc_mask;\n\
         layout(set = 0, binding = 4) uniform texture2D vjc_prev;\n\
         #define PREV_PIXEL(uv) textureLod(sampler2D(vjc_prev, isf_smp), vec2((uv).x, 1.0 - (uv).y), 0.0)\n\
         #define PREV_THIS_PIXEL PREV_PIXEL(isf_FragNormCoord)\n\
         #define gl_FragColor isf_FragColor\n\
         #define TIME isf_u.p[0].x\n\
         #define RENDERSIZE isf_u.p[0].yz\n\
         #define IMG_NORM_PIXEL(img, uv) textureLod(sampler2D(img, isf_smp), vec2((uv).x, 1.0 - (uv).y), 0.0)\n\
         #define IMG_THIS_PIXEL(img) IMG_NORM_PIXEL(img, isf_FragNormCoord)\n\
         #define IMG_PIXEL(img, px) IMG_NORM_PIXEL(img, (px) / RENDERSIZE)\n\
         #define IMG_SIZE(img) RENDERSIZE\n",
    );
    if isf.passes.is_empty() {
        s.push_str("#define NEW_TICK isf_u.p[0].w\n#define PASSINDEX 0\n");
    } else {
        // Slot 0 .w carries the pass index instead of the tick flag.
        s.push_str("#define NEW_TICK 0.0\n#define PASSINDEX int(isf_u.p[0].w + 0.5)\n");
        for (k, t) in isf.targets().iter().enumerate() {
            s.push_str(&format!(
                "layout(set = 0, binding = {}) uniform texture2D {t};\n",
                TARGET_BINDING as usize + k
            ));
        }
    }
    if isf.audio.iter().any(|a| a.fft) {
        s.push_str(&format!(
            "layout(set = 0, binding = {}) uniform texture2D vjc_audio_fft;\n",
            AUDIO_BINDING
        ));
    }
    if isf.audio.iter().any(|a| !a.fft) {
        s.push_str(&format!(
            "layout(set = 0, binding = {}) uniform texture2D vjc_audio_wave;\n",
            AUDIO_BINDING + 1
        ));
    }
    for a in &isf.audio {
        s.push_str(&format!(
            "#define {} {}\n",
            a.name,
            if a.fft {
                "vjc_audio_fft"
            } else {
                "vjc_audio_wave"
            }
        ));
    }
    for (n, i) in isf.inputs_iter() {
        let slot = format!("isf_u.p[{n}]");
        let expr = match &i.kind {
            InputKind::Float { .. } => format!("{slot}.x"),
            InputKind::Bool { .. } => format!("({slot}.x > 0.5)"),
            InputKind::Color { .. } => slot,
            InputKind::Point2D { .. } => format!("{slot}.xy"),
            InputKind::Long { .. } => format!("int(floor({slot}.x + 0.5))"),
            InputKind::Text { .. } => n.to_string(),
        };
        s.push_str(&format!("#define vjc_u_{} {}\n", i.name, expr));
    }
    let names: Vec<&str> = isf.inputs.iter().map(|i| i.name.as_str()).collect();
    s.push_str(&rename_inputs(&isf.body, &names));
    s
}

impl Isf {
    /// Distinct `TARGET` names in first-use order; index k is bound at `TARGET_BINDING + k`.
    pub fn targets(&self) -> Vec<&str> {
        targets_of(&self.passes)
    }

    /// The effect needs a per-instance id: a persistent flag or a persistent target.
    pub fn needs_instance(&self) -> bool {
        self.persistent || self.passes.iter().any(|p| p.persistent)
    }

    /// First uniform slot of every input (slot 0 is TIME/RENDERSIZE).
    fn first_slots(&self) -> Vec<usize> {
        let mut n = 1;
        self.inputs
            .iter()
            .map(|i| {
                let at = n;
                n += i.kind.slots();
                at
            })
            .collect()
    }

    fn inputs_iter(&self) -> impl Iterator<Item = (usize, &Input)> {
        self.first_slots().into_iter().zip(&self.inputs)
    }
}

/// Translates ISF to a WGSL fragment module (entry point `main`; bindings: 0 uniform
/// block, 1 input texture, 2 sampler, 3 ML mask texture (R8, only used by ML effects), 4 previous output of this instance (persistent effects only; blank otherwise), 5.. named PASSES targets (`targets()` order), `AUDIO_BINDING` spectrum and +1 waveform textures (only declared when the shader has audio inputs); `@location(0)` input is the ISF y-up uv).
pub fn translate(isf: &Isf) -> Result<String, FxError> {
    use naga::{back::wgsl, front::glsl, valid};
    let names: Vec<&str> = isf.inputs.iter().map(|i| i.name.as_str()).collect();
    if let Some(n) = declared_input(&isf.body, &names) {
        return err(format!(
            "input {n} is also declared as a variable or member in the shader; rename one"
        ));
    }
    let src = glsl_source(isf);
    let module = glsl::Frontend::default()
        .parse(&glsl::Options::from(naga::ShaderStage::Fragment), &src)
        .map_err(|e| FxError(format!("GLSL: {e:?}")))?;
    let info = valid::Validator::new(valid::ValidationFlags::all(), valid::Capabilities::all())
        .validate(&module)
        .map_err(|e| FxError(format!("validation: {e:?}")))?;
    wgsl::write_string(&module, &info, wgsl::WriterFlags::empty())
        .map_err(|e| FxError(format!("WGSL: {e:?}")))
}

/// Parses and translates in one go.
pub fn compile(src: &str) -> Result<(Isf, String), FxError> {
    let isf = parse(src)?;
    let wgsl = translate(&isf)?;
    Ok((isf, wgsl))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_body(body: &str, input: &str) -> Isf {
        parse(&format!(
            r#"/*{{"INPUTS":[{{"NAME":"inputImage","TYPE":"image"}},{{"NAME":"{input}","TYPE":"float","MIN":0,"MAX":1,"DEFAULT":0.5}}]}}*/
{body}"#
        ))
        .unwrap()
    }

    #[test]
    fn extra_glsl_keywords_are_rejected_as_input_names() {
        for n in [
            "switch",
            "case",
            "default",
            "precision",
            "highp",
            "bvec3",
            "uvec2",
            "mat2x3",
        ] {
            let src = format!(
                r#"/*{{"INPUTS":[{{"NAME":"{n}","TYPE":"float","MIN":0,"MAX":1,"DEFAULT":0}}]}}*/
void main() {{ gl_FragColor = vec4(1.0); }}"#
            );
            assert!(parse(&src).is_err(), "{n}");
        }
    }

    #[test]
    fn input_redeclared_in_body_is_a_clear_error() {
        let ok = with_body(
            "void main() { vec4 q = vec4(amount); gl_FragColor = q.xxxw; }",
            "amount",
        );
        assert!(translate(&ok).is_ok());
        for body in [
            "void main() { float amount = 1.0; gl_FragColor = vec4(amount); }",
            "float f(float amount) { return amount; }\nvoid main() { gl_FragColor = vec4(f(1.0)); }",
            "struct S { vec2 amount; };\nvoid main() { gl_FragColor = vec4(1.0); }",
        ] {
            let e = translate(&with_body(body, "amount"))
                .unwrap_err()
                .to_string();
            assert!(e.contains("amount") && e.contains("declared"), "{e}");
        }
        // Member access and comments do not count.
        let m = with_body(
            "// float amount\nvoid main() { vec4 v = vec4(1.0); gl_FragColor = vec4(v.x * amount); }",
            "x2",
        );
        assert!(declared_input(&m.body, &["x"]).is_none());
    }

    #[test]
    fn declared_input_sees_declarator_lists() {
        let n = ["amount"];
        for b in [
            "void main() { float a, amount; }",
            "void main() { float a = 1.0, amount = 2.0; }",
            "void main() { float a = mix(0.0, 1.0, 0.5), b, amount; }",
            "void main() { float a = float(1.0), amount; }",
            "void main() { float a = float(1.0), amount = 2.0; }",
            "void main() { vec2 a = vec2(float(1.0), float(2.0)), amount; }",
            "void main() { float a = float(float(1.0)), amount; }",
            "void main() { float a = float (1.0), amount; }",
            "void main() { float a = float(1.0) + float(2.0), amount; }",
        ] {
            assert_eq!(declared_input(b, &n).as_deref(), Some("amount"), "{b}");
        }
        // A user struct type declares variables too.
        let b = "struct Light { float k; }; void main() { Light amount; }";
        assert_eq!(declared_input(b, &n).as_deref(), Some("amount"));
        let b = "struct Light { float k; }; void main() { Light l; x = amount; }";
        assert_eq!(declared_input(b, &n), None);
        // Uses in calls, initializers and later statements are not declarations.
        for b in [
            "void main() { float a = f(1.0, amount); }",
            "void main() { float a, b; c = vec2(a, amount); }",
            "void main() { float a; x = a, amount; }",
        ] {
            assert_eq!(declared_input(b, &n), None, "{b}");
        }
    }

    fn inp(name: &str, group: Option<&str>) -> Input {
        Input {
            name: name.into(),
            kind: InputKind::Bool { default: false },
            group: group.map(String::from),
        }
    }

    #[test]
    fn groups_come_from_group_key_or_shared_name_prefix() {
        let v = [
            inp("mix", None),
            inp("blur_amount", None),
            inp("blur_angle", None),
            inp("tint", Some("Color")),
            inp("gain", Some("Color")),
            inp("solo_one", None),
        ];
        let g = param_groups(&v);
        assert_eq!(g[0], (None, vec![0, 5]));
        assert_eq!(g[1], (Some("Blur".into()), vec![1, 2]));
        assert_eq!(g[2], (Some("Color".into()), vec![3, 4]));
        assert_eq!(g.len(), 3);
    }

    #[test]
    fn negative_control_no_shared_names_means_no_sections() {
        let v = [inp("a_x", None), inp("b_y", None), inp("c", None)];
        assert_eq!(param_groups(&v), vec![(None, vec![0, 1, 2])]);
    }

    #[test]
    fn group_key_is_parsed_from_the_header() {
        let src = r#"/*{"INPUTS":[{"NAME":"inputImage","TYPE":"image"},
          {"NAME":"a","TYPE":"bool","GROUP":"Look"}]}*/
          void main() { gl_FragColor = IMG_THIS_PIXEL(inputImage); }"#;
        assert_eq!(parse(src).unwrap().inputs[0].group.as_deref(), Some("Look"));
    }

    #[test]
    fn parses_all_input_types() {
        let src = r#"/*{
          "DESCRIPTION": "d", "CATEGORIES": ["A", "B"],
          "INPUTS": [
            {"NAME": "inputImage", "TYPE": "image"},
            {"NAME": "f", "TYPE": "float", "MIN": 0, "MAX": 2, "DEFAULT": 0.5},
            {"NAME": "b", "TYPE": "bool", "DEFAULT": true},
            {"NAME": "c", "TYPE": "color", "DEFAULT": [0.1, 0.2, 0.3, 1.0]},
            {"NAME": "p", "TYPE": "point2D", "DEFAULT": [0.25, 0.75]},
            {"NAME": "l", "TYPE": "long", "VALUES": [3, 7], "LABELS": ["x", "y"], "DEFAULT": 7}
          ]}*/
          void main() { gl_FragColor = IMG_THIS_PIXEL(inputImage); }"#;
        let isf = parse(src).unwrap();
        assert_eq!(isf.description, "d");
        assert_eq!(isf.categories, ["A", "B"]);
        assert_eq!(isf.inputs.len(), 5);
        assert_eq!(
            isf.inputs[0].kind,
            InputKind::Float {
                min: 0.0,
                max: 2.0,
                default: 0.5
            }
        );
        assert_eq!(isf.inputs[1].kind, InputKind::Bool { default: true });
        assert_eq!(
            isf.inputs[2].kind,
            InputKind::Color {
                default: [0.1, 0.2, 0.3, 1.0]
            }
        );
        assert!(matches!(
            isf.inputs[3].kind,
            InputKind::Point2D {
                default: [0.25, 0.75],
                ..
            }
        ));
        assert!(matches!(
            &isf.inputs[4].kind,
            InputKind::Long { default: 1, .. }
        ));
        let u = isf.pack(&isf.default_params(), 2.0, (8, 4));
        assert_eq!(u[0], [2.0, 8.0, 4.0, 0.0]);
        assert_eq!(u[1][0], 0.5);
        assert_eq!(u[2][0], 1.0);
        assert_eq!(u[5][0], 7.0);
        translate(&isf).unwrap();
    }

    #[test]
    fn bad_input_is_error_not_panic() {
        assert!(parse("void main() {}").is_err());
        assert!(parse("/*{ nope }*/").is_err());
        assert!(parse(r#"/*{"INPUTS":[{"NAME":"x","TYPE":"weird"}]}*/"#).is_err());
        let isf = parse(r#"/*{}*/ void main() { gl_FragColor = nonsense(; }"#).unwrap();
        assert!(translate(&isf).is_err());
        let isf = parse("/*{}*/ void main() { gl_FragColor = vec3(1.0); }").unwrap();
        assert!(translate(&isf).is_err());
    }

    const AUDIO_SRC: &str = r#"/*{"INPUTS":[
        {"NAME":"spec","TYPE":"audioFFT","MAX":32},
        {"NAME":"wave","TYPE":"audio","MAX":9999},
        {"NAME":"gain","TYPE":"float","MIN":0,"MAX":2,"DEFAULT":1}]}*/
void main() {
    float a = IMG_NORM_PIXEL(spec, vec2(isf_FragNormCoord.x, 0.5)).r;
    float w = IMG_NORM_PIXEL(wave, vec2(isf_FragNormCoord.x, 0.5)).r;
    gl_FragColor = vec4(vec3(a * gain, w, 0.0), 1.0);
}"#;

    #[test]
    fn audio_inputs_parse_and_are_not_uniforms() {
        let isf = parse(AUDIO_SRC).unwrap();
        assert_eq!(isf.inputs.len(), 1, "only gain is a uniform");
        assert_eq!(
            isf.audio,
            vec![
                AudioInput {
                    name: "spec".into(),
                    fft: true,
                    samples: 32
                },
                AudioInput {
                    name: "wave".into(),
                    fft: false,
                    samples: AUDIO_TEX_W
                },
            ]
        );
        let dup = AUDIO_SRC.replace(r#""NAME":"wave""#, r#""NAME":"spec""#);
        assert!(parse(&dup).is_err());
    }

    #[test]
    fn audio_inputs_bind_textures() {
        let wgsl = translate(&parse(AUDIO_SRC).unwrap()).unwrap();
        for b in [AUDIO_BINDING, AUDIO_BINDING + 1] {
            assert!(
                wgsl.contains(&format!("@binding({b})")),
                "binding {b}\n{wgsl}"
            );
        }
        // A shader with no audio input declares neither.
        let plain = translate(&parse(BUILTIN[0].1).unwrap()).unwrap();
        assert!(!plain.contains(&format!("@binding({AUDIO_BINDING})")));
    }

    #[test]
    fn audio_texels_stretch_and_flag_activity() {
        assert!(audio_texels(&[1.0], false).iter().all(|b| *b == 0));
        let t = audio_texels(&[0.0, 1.0], true);
        assert_eq!(t.len(), AUDIO_TEX_W * 4);
        assert_eq!((t[0], t[3]), (0, 255));
        assert_eq!(
            (t[(AUDIO_TEX_W - 1) * 4], t[AUDIO_TEX_W * 4 - 1]),
            (255, 255)
        );
    }

    #[test]
    fn all_builtin_effects_translate() {
        assert!(BUILTIN.len() >= 50);
        for (id, src) in BUILTIN {
            let (isf, wgsl) = compile(src).unwrap_or_else(|e| panic!("{id}: {e}"));
            assert!(wgsl.contains("fn main"), "{id}");
            let e = new_effect(id).unwrap();
            assert_eq!(e.params.len(), isf.inputs.len());
            // The WGSL parses back and validates.
            let m = naga::front::wgsl::parse_str(&wgsl).unwrap_or_else(|e| panic!("{id}: {e:?}"));
            naga::valid::Validator::new(
                naga::valid::ValidationFlags::all(),
                naga::valid::Capabilities::all(),
            )
            .validate(&m)
            .unwrap_or_else(|e| panic!("{id}: {e:?}"));
        }
    }

    #[test]
    fn all_generators_compile() {
        assert!(GENERATORS.len() >= 12);
        for (id, src) in GENERATORS {
            let (isf, wgsl) = compile(src).unwrap_or_else(|e| panic!("{id}: {e}"));
            assert!(wgsl.contains("fn main"), "{id}");
            assert_eq!(generator_params(id).unwrap().len(), isf.inputs.len());
            let m = naga::front::wgsl::parse_str(&wgsl).unwrap_or_else(|e| panic!("{id}: {e:?}"));
            naga::valid::Validator::new(
                naga::valid::ValidationFlags::all(),
                naga::valid::Capabilities::all(),
            )
            .validate(&m)
            .unwrap_or_else(|e| panic!("{id}: {e:?}"));
        }
        assert!(generator_params("invert").is_none());
    }

    /// The fx-pack-1 files all parse, have unique ids and are registered under their file name.
    #[test]
    fn fx_pack_1_parses_with_unique_ids() {
        let ids: Vec<&str> = BUILTIN.iter().chain(GENERATORS).map(|(k, _)| *k).collect();
        let mut sorted = ids.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), ids.len(), "duplicate ids");
        for id in [
            "exposure_gamma",
            "vibrance",
            "color_balance",
            "sepia",
            "black_white",
            "colorize",
            "thermal",
            "night_vision",
            "sketch",
            "oil_paint",
            "cartoon",
            "bloom",
            "fisheye",
            "pinch",
            "vortex",
            "shake",
            "flip",
            "rotate_zoom",
            "mirror_quad",
            "infinite_zoom",
            "tunnel",
            "metaballs",
            "fire",
            "clouds",
            "lines",
            "circles_grid",
            "triangles",
            "lens_flare",
            "particles",
            "gradient_sweep",
        ] {
            let src = builtin_source(id).unwrap_or_else(|| panic!("{id} not registered"));
            parse(src).unwrap_or_else(|e| panic!("{id}: {e}"));
        }
    }

    /// The fx-pack-2 files all parse, have unique ids and are registered under their file name.
    #[test]
    fn fx_pack_2_parses_with_unique_ids() {
        let ids: Vec<&str> = BUILTIN.iter().chain(GENERATORS).map(|(k, _)| *k).collect();
        let mut sorted = ids.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), ids.len(), "duplicate ids");
        for id in [
            "add_subtract",
            "color_pass",
            "hue_rotate_cycle",
            "contrast_curve",
            "gamma_rgb",
            "exposure_flash",
            "edge_glow",
            "displace_map",
            "wave_warp",
            "stripes_overlay",
            "slide",
            "radial_mosaic",
            "stingy_sphere",
            "goo",
            "fragment",
            "iterate",
            "shift_glitch",
            "lowres",
            "shadow_drop",
            "outline",
            "cross_hatch",
            "rain_drops",
            "spectrum_bars",
            "waveform_line",
            "grid_3d",
            "hex_grid",
            "spiral",
            "lissajous",
            "stars_warp",
            "lava_lamp",
        ] {
            let src = builtin_source(id).unwrap_or_else(|| panic!("{id} not registered"));
            let isf = parse(src).unwrap_or_else(|e| panic!("{id}: {e}"));
            assert!(!isf.categories.is_empty(), "{id}: no categories");
        }
    }

    /// Every .fs under assets/ (registered or not) must go ISF -> WGSL -> naga validate,
    /// be registered, and carry a category, a description and sane ranges. All failures
    /// are listed by name in one message.
    #[test]
    fn every_fs_file_compiles_and_is_registered() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets");
        let mut failures = vec![];
        let mut seen = 0;
        for dir in ["effects", "generators"] {
            let mut files: Vec<_> = std::fs::read_dir(root.join(dir))
                .unwrap()
                .map(|e| e.unwrap().path())
                .filter(|p| p.extension().is_some_and(|x| x == "fs"))
                .collect();
            files.sort();
            for path in files {
                seen += 1;
                let id = path.file_stem().unwrap().to_string_lossy().to_string();
                let src = std::fs::read_to_string(&path).unwrap();
                let check = || -> Result<(), String> {
                    let (isf, wgsl) = compile(&src).map_err(|e| e.to_string())?;
                    let m = naga::front::wgsl::parse_str(&wgsl).map_err(|e| format!("{e:?}"))?;
                    naga::valid::Validator::new(
                        naga::valid::ValidationFlags::all(),
                        naga::valid::Capabilities::all(),
                    )
                    .validate(&m)
                    .map_err(|e| format!("{e:?}"))?;
                    if isf.categories.is_empty() || isf.description.is_empty() {
                        return Err("missing category or description".into());
                    }
                    for i in &isf.inputs {
                        if let InputKind::Float { min, max, default } = i.kind
                            && (min >= max || default < min || default > max)
                        {
                            return Err(format!("bad range on {}", i.name));
                        }
                    }
                    let list = if dir == "effects" {
                        BUILTIN
                    } else {
                        GENERATORS
                    };
                    if !list.iter().any(|(k, _)| *k == id) {
                        return Err("not registered in lib.rs".into());
                    }
                    Ok(())
                };
                if let Err(e) = check() {
                    failures.push(format!("{dir}/{id}.fs: {e}"));
                }
            }
        }
        assert_eq!(seen, BUILTIN.len() + GENERATORS.len(), "registry vs files");
        assert!(failures.is_empty(), "failed:\n{}", failures.join("\n"));
    }

    #[test]
    fn input_names_cannot_shadow_builtins() {
        for n in ["TIME", "main", "vec4", "isf_u", "gl_Position"] {
            let src = format!(
                r#"/*{{"INPUTS":[{{"NAME":"{n}","TYPE":"float"}}]}}*/ void main() {{ gl_FragColor = vec4(1.0); }}"#
            );
            assert!(parse(&src).is_err(), "{n} accepted");
        }
    }

    #[test]
    fn input_named_like_glsl_function_works() {
        let src = r#"/*{"INPUTS":[{"NAME":"mix","TYPE":"float"},{"NAME":"length","TYPE":"float"}]}*/
          // mix in a comment
          void main() { float a = mix(0.0, 1.0, mix) * length; gl_FragColor = vec4(vec3(a + length(vec2(1.0))), 1.0); }"#;
        let isf = parse(src).unwrap();
        let g = glsl_source(&isf);
        assert!(g.contains("mix(0.0, 1.0, vjc_u_mix) * vjc_u_length"));
        assert!(g.contains("// mix in a comment"));
        translate(&isf).unwrap();
        let src = r#"/*{"INPUTS":[{"NAME":"TIME","TYPE":"float"}]}*/ void main() {}"#;
        assert!(parse(src).is_err());
    }

    #[test]
    fn non_finite_params_pack_as_zero() {
        let src = r#"/*{"INPUTS":[{"NAME":"c","TYPE":"color"},{"NAME":"x","TYPE":"float"}]}*/
          void main() { gl_FragColor = c; }"#;
        let isf = parse(src).unwrap();
        let mut p = isf.default_params();
        p[0].value = Value::Color([f32::NAN, f32::INFINITY, 0.5, 1.0]);
        p[1].value = Value::Number(f64::NAN);
        let u = isf.pack(&p, 0.0, (1, 1));
        assert_eq!(u[1], [0.0, 0.0, 0.5, 1.0]);
        assert_eq!(u[2], [0.0; 4]);
    }

    /// #12: new text sources carry rotation, the long text input packs 24 characters, and
    /// the cloners / video wall / strokes / extra generators are registered.
    #[test]
    fn wave_o_sources_are_registered_with_their_params() {
        for id in ["text", "text_block", "text_animator"] {
            let (isf, _) = compile(builtin_source(id).unwrap()).unwrap();
            assert!(isf.inputs.iter().any(|i| i.name == "rotation"), "{id}");
        }
        let (anim, _) = compile(builtin_source("text_animator").unwrap()).unwrap();
        let preset = anim.inputs.iter().find(|i| i.name == "preset").unwrap();
        assert!(matches!(&preset.kind, InputKind::Long { values, .. } if values.len() == 5));
        let mut params = anim.default_params();
        params[0].value = Value::Text("AB|".repeat(8));
        let u = anim.pack(&params, 0.0, (64, 64));
        assert_eq!(TEXT_CHARS, 24);
        assert_eq!(
            u[TEXT_SLOTS][3], 124.0,
            "24th character reaches the last slot"
        );
        for id in [
            "cloner_grid",
            "cloner_linear",
            "cloner_radial",
            "video_wall",
            "strokes",
        ] {
            assert!(BUILTIN.iter().any(|(k, _)| *k == id), "{id}");
        }
        for id in [
            "test_card",
            "abstract_field",
            "stroboscope",
            "shaper",
            "text_block",
            "text_animator",
        ] {
            assert!(generator_params(id).is_some(), "{id}");
        }
        // Negative control: an unregistered name is not found.
        assert!(generator_params("cloner_grid").is_none());
    }

    #[test]
    fn text_input_packs_character_codes_over_four_slots() {
        let (isf, _) = compile(builtin_source("text").unwrap()).unwrap();
        assert!(!isf.persistent);
        let mut params = isf.default_params();
        params[0].value = Value::Text("Hi!".into());
        let u = isf.pack(&params, 0.0, (64, 64));
        assert_eq!(u[1], [72.0, 105.0, 33.0, 32.0]);
        // Padding is spaces; the next input starts after the 4 text slots.
        assert_eq!(u[TEXT_SLOTS], [32.0; 4]);
        assert_eq!(u[1 + TEXT_SLOTS], [0.0, 0.0, 0.0, 1.0]);
        // Negative control: another string gives other codes.
        params[0].value = Value::Text("Z".into());
        assert_eq!(isf.pack(&params, 0.0, (64, 64))[1][0], 90.0);
        // Non-ASCII and over-long input never overflows or yields NaN.
        params[0].value = Value::Text("é".repeat(40));
        let u = isf.pack(&params, 0.0, (64, 64));
        assert!(u[1..=TEXT_SLOTS].iter().flatten().all(|c| *c == 32.0));
        // A mistyped value falls back to the default text.
        params[0].value = Value::Number(1.0);
        assert_eq!(isf.pack(&params, 0.0, (64, 64))[1][0], 86.0);
    }

    #[test]
    fn tick_flag_sees_any_tick_change_even_an_even_jump() {
        // Rate 1: ticks are whole seconds.
        assert_eq!(tick_flag(0.2, 1.0, None), (2.0, 0));
        assert_eq!(tick_flag(0.7, 1.0, Some(0)), (0.0, 0), "same tick: keep");
        assert_eq!(tick_flag(1.1, 1.0, Some(0)), (1.0, 1));
        // TIME jumped two ticks (0 -> 2): the parity of the tick is the same, but it is new.
        assert_eq!(tick_flag(2.4, 1.0, Some(0)), (1.0, 2));
        // The clock went back.
        assert_eq!(tick_flag(0.1, 1.0, Some(5)), (1.0, 0));
    }

    #[test]
    fn tick_input_is_packed_as_the_rate_and_must_name_a_float() {
        let src = r#"/*{"PERSISTENT": true, "TICK_INPUT": "fps", "INPUTS":[{"NAME":"fps","TYPE":"float","MIN":0.5,"MAX":60.0,"DEFAULT":4.0}]}*/ void main(){ gl_FragColor = NEW_TICK > 0.5 ? vec4(1.0) : PREV_THIS_PIXEL; }"#;
        let isf = parse(src).unwrap();
        assert_eq!(isf.tick_input.as_deref(), Some("fps"));
        assert!(translate(&isf).is_ok());
        assert_eq!(isf.pack(&isf.default_params(), 3.0, (8, 8))[0][3], 4.0);
        // Control: an effect without the header packs no rate.
        let plain =
            parse(r#"/*{"INPUTS":[{"NAME":"fps","TYPE":"float"}]}*/ void main(){}"#).unwrap();
        assert_eq!(plain.tick_input, None);
        assert_eq!(plain.pack(&plain.default_params(), 3.0, (8, 8))[0][3], 0.0);
        // Naming something that is not a float input is ignored.
        let bad = parse(r#"/*{"TICK_INPUT": "nope"}*/ void main(){}"#).unwrap();
        assert_eq!(bad.tick_input, None);
    }

    #[test]
    fn persistent_header_flag_is_read() {
        let on =
            parse(r#"/*{"PERSISTENT": true}*/ void main(){ gl_FragColor = PREV_THIS_PIXEL; }"#)
                .unwrap();
        assert!(on.persistent);
        assert!(translate(&on).is_ok());
        let off = parse(r#"/*{}*/ void main(){ gl_FragColor = vec4(1.0); }"#).unwrap();
        assert!(!off.persistent);
    }

    fn passes_src(passes: &str) -> String {
        format!(
            r#"/*{{"INPUTS":[{{"NAME":"inputImage","TYPE":"image"}},{{"NAME":"amt","TYPE":"float"}}],"PASSES":{passes}}}*/
void main() {{ gl_FragColor = PASSINDEX == 0 ? IMG_THIS_PIXEL(inputImage) : IMG_THIS_PIXEL(buf); }}"#
        )
    }

    #[test]
    fn passes_are_parsed_with_sizes_and_flags() {
        let isf = parse(&passes_src(
            r#"[{"TARGET":"buf","PERSISTENT":true,"FLOAT":true,"WIDTH":"$WIDTH/2","HEIGHT":"($HEIGHT + 2) * 0.5"},{}]"#,
        ))
        .unwrap();
        assert_eq!(isf.passes.len(), 2);
        let p = &isf.passes[0];
        assert_eq!(p.target.as_deref(), Some("buf"));
        assert!(p.persistent && p.float);
        assert_eq!(p.size((640, 480)), (320, 241));
        assert_eq!(isf.passes[1].target, None);
        assert_eq!(isf.passes[1].size((640, 480)), (640, 480));
        assert_eq!(isf.targets(), ["buf"]);
        assert!(isf.needs_instance());
        let plain = parse(&passes_src(r#"[{"TARGET":"buf"},{}]"#)).unwrap();
        assert!(!plain.passes[0].persistent && !plain.needs_instance());
        assert!(parse("/*{}*/ void main(){}").unwrap().passes.is_empty());
    }

    #[test]
    fn size_expressions() {
        assert_eq!(eval_size("$WIDTH", (100, 50)), Ok(100.0));
        assert_eq!(eval_size("$HEIGHT/2 + 1", (100, 50)), Ok(26.0));
        assert_eq!(eval_size("2 * (3 + 4) - 1", (1, 1)), Ok(13.0));
        assert_eq!(eval_size("-$WIDTH + 10", (4, 1)), Ok(6.0));
        for bad in ["", "$FOO", "1 +", "(1", "1 / 0", "abc", "1 2"] {
            assert!(eval_size(bad, (10, 10)).is_err(), "{bad}");
        }
    }

    #[test]
    fn bad_pass_specs_are_rejected_with_a_clear_error() {
        for (passes, needle) in [
            ("{}", "must be an array"),
            ("[]", "empty"),
            (r#"[1]"#, "not an object"),
            (r#"[{"TARGET":3},{}]"#, "TARGET must be a string"),
            (r#"[{"TARGET":"a b"},{}]"#, "invalid TARGET"),
            (r#"[{"TARGET":"amt"},{}]"#, "invalid TARGET"),
            (r#"[{"TARGET":"inputImage"},{}]"#, "invalid TARGET"),
            (r#"[{"PERSISTENT":true},{}]"#, "needs a TARGET"),
            (
                r#"[{"TARGET":"buf","WIDTH":"$NOPE"},{}]"#,
                "unknown variable",
            ),
            (r#"[{"TARGET":"buf","WIDTH":"1 +"},{}]"#, "WIDTH"),
            (r#"[{"TARGET":"buf","HEIGHT":"0"},{}]"#, "not a positive"),
            (r#"[{"TARGET":"buf"}]"#, "last pass"),
            (
                r#"[{"TARGET":"a"},{"TARGET":"b"},{"TARGET":"c"},{"TARGET":"d"},{"TARGET":"e"},{}]"#,
                "too many TARGET",
            ),
        ] {
            let e = parse(&passes_src(passes)).unwrap_err().to_string();
            assert!(e.contains(needle), "{passes}: {e}");
        }
        let many = format!("[{}{{}}]", r#"{"TARGET":"a"},"#.repeat(MAX_PASSES));
        assert!(
            parse(&passes_src(&many))
                .unwrap_err()
                .0
                .contains("too many PASSES")
        );
        let e = parse(r#"/*{"PERSISTENT":true,"PASSES":[{}]}*/ void main(){}"#).unwrap_err();
        assert!(e.0.contains("PERSISTENT with PASSES"), "{e}");
        let e = parse(
            r#"/*{"TICK_INPUT":"f","INPUTS":[{"NAME":"f","TYPE":"float"}],"PASSES":[{}]}*/ void main(){}"#,
        )
        .unwrap_err();
        assert!(e.0.contains("TICK_INPUT"), "{e}");
    }

    #[test]
    fn passindex_and_named_targets_translate() {
        let isf = parse(&passes_src(r#"[{"TARGET":"buf"},{}]"#)).unwrap();
        let g = glsl_source(&isf);
        assert!(g.contains("binding = 5) uniform texture2D buf;"), "{g}");
        assert!(g.contains("#define PASSINDEX int(isf_u.p[0].w"));
        let wgsl = translate(&isf).unwrap();
        assert!(wgsl.contains("@binding(5)"), "{wgsl}");
        // Single-pass effects see PASSINDEX 0 and keep NEW_TICK.
        let one =
            parse(r#"/*{}*/ void main(){ gl_FragColor = vec4(float(PASSINDEX) + NEW_TICK); }"#)
                .unwrap();
        let g = glsl_source(&one);
        assert!(g.contains("#define PASSINDEX 0") && g.contains("#define NEW_TICK isf_u.p[0].w"));
        translate(&one).unwrap();
    }

    #[test]
    fn bundled_multipass_effects_declare_passes() {
        for (id, persistent) in [("blur_separable", false), ("feedback_trail", true)] {
            let isf = parse(builtin_source(id).unwrap()).unwrap();
            assert_eq!(isf.passes.len(), 2, "{id}");
            assert_eq!(isf.needs_instance(), persistent, "{id}");
        }
    }
}
