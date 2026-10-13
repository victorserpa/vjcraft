//! ISF effects: header parser and GLSL -> WGSL translation through naga.
//!
//! An ISF file is a JSON header in a `/*{ ... }*/` comment followed by a GLSL fragment
//! body. Only single-pass image filters are supported (no PASSES, no persistent buffers).
//! Everything here is pure CPU work and returns errors instead of panicking, so the app
//! can run it on a worker thread and disable the effect on failure.

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
    (
        "brightness_contrast",
        include_str!("../../../assets/effects/brightness_contrast.fs"),
    ),
];

/// Source of a bundled effect.
pub fn builtin_source(id: &str) -> Option<&'static str> {
    BUILTIN.iter().find(|(k, _)| *k == id).map(|(_, s)| *s)
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
}

#[derive(Debug, Clone, PartialEq)]
pub struct Input {
    pub name: String,
    pub kind: InputKind,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Isf {
    pub description: String,
    pub categories: Vec<String>,
    /// Controllable inputs, in file order (the image input is not listed).
    pub inputs: Vec<Input>,
    /// GLSL after the header.
    pub body: String,
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
    let mut inputs = vec![];
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
            other => return err(format!("unsupported input type {other} ({name})")),
        };
        if !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            || name.starts_with(|c: char| c.is_ascii_digit())
        {
            return err(format!("invalid input name {name}"));
        }
        inputs.push(Input { name, kind });
    }
    if inputs.len() >= MAX_SLOTS {
        return err("too many inputs");
    }
    Ok(Isf {
        description: str_of("DESCRIPTION"),
        categories,
        inputs,
        body: src[end + 2..].to_string(),
    })
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

    /// Fills the uniform block: slot 0 = (TIME, width, height, 0), then one slot per input.
    /// Params are matched by name; missing or mistyped ones fall back to the default.
    pub fn pack(&self, params: &[Param], time: f32, size: (u32, u32)) -> Uniforms {
        let mut u = [[0.0; 4]; MAX_SLOTS];
        u[0] = [time, size.0 as f32, size.1 as f32, 0.0];
        let defaults = self.default_params();
        for (n, (inp, def)) in self.inputs.iter().zip(&defaults).enumerate() {
            let v = params
                .iter()
                .find(|p| p.name == inp.name)
                .map_or(&def.value, |p| &p.value);
            u[n + 1] = match (&inp.kind, v) {
                (InputKind::Float { .. }, Value::Number(x)) => [*x as f32, 0.0, 0.0, 0.0],
                (InputKind::Bool { .. }, Value::Bool(b)) => [*b as u8 as f32, 0.0, 0.0, 0.0],
                (InputKind::Color { .. }, Value::Color(c)) => *c,
                (InputKind::Point2D { .. }, Value::Color(c)) => [c[0], c[1], 0.0, 0.0],
                (InputKind::Long { values, .. }, Value::Choice { selected, .. }) => {
                    let i = (*selected as usize).min(values.len() - 1);
                    [values[i] as f32, 0.0, 0.0, 0.0]
                }
                _ => pack_default(&def.value),
            };
        }
        u
    }
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
         #define gl_FragColor isf_FragColor\n\
         #define TIME isf_u.p[0].x\n\
         #define RENDERSIZE isf_u.p[0].yz\n\
         #define IMG_NORM_PIXEL(img, uv) textureLod(sampler2D(img, isf_smp), vec2((uv).x, 1.0 - (uv).y), 0.0)\n\
         #define IMG_THIS_PIXEL(img) IMG_NORM_PIXEL(img, isf_FragNormCoord)\n\
         #define IMG_PIXEL(img, px) IMG_NORM_PIXEL(img, (px) / RENDERSIZE)\n\
         #define IMG_SIZE(img) RENDERSIZE\n",
    );
    for (n, i) in isf.inputs_iter() {
        let slot = format!("isf_u.p[{n}]");
        let expr = match &i.kind {
            InputKind::Float { .. } => format!("{slot}.x"),
            InputKind::Bool { .. } => format!("({slot}.x > 0.5)"),
            InputKind::Color { .. } => slot,
            InputKind::Point2D { .. } => format!("{slot}.xy"),
            InputKind::Long { .. } => format!("int(floor({slot}.x + 0.5))"),
        };
        s.push_str(&format!("#define {} {}\n", i.name, expr));
    }
    s.push_str(&isf.body);
    s
}

impl Isf {
    fn inputs_iter(&self) -> impl Iterator<Item = (usize, &Input)> {
        self.inputs.iter().enumerate().map(|(n, i)| (n + 1, i))
    }
}

/// Translates ISF to a WGSL fragment module (entry point `main`; bindings: 0 uniform
/// block, 1 input texture, 2 sampler; `@location(0)` input is the ISF y-up uv).
pub fn translate(isf: &Isf) -> Result<String, FxError> {
    use naga::{back::wgsl, front::glsl, valid};
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

    #[test]
    fn all_builtin_effects_translate() {
        assert_eq!(BUILTIN.len(), 10);
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
}
