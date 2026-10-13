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
];

/// Source of a bundled effect or generator.
pub fn builtin_source(id: &str) -> Option<&'static str> {
    BUILTIN
        .iter()
        .chain(GENERATORS)
        .find(|(k, _)| *k == id)
        .map(|(_, s)| *s)
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
            || RESERVED.contains(&name.as_str())
            || name.starts_with("isf_")
            || name.starts_with("vjc_u_")
            || name.starts_with("gl_")
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

/// Names an input must not take. Inputs are declared as `#define vjc_u_<name> expr` and
/// the shader body is rewritten (see `rename_inputs`), so ordinary function names such as
/// `mix` or `length` are fine as input names. Only ISF builtins, names the generator
/// declares, and GLSL keywords/types (which cannot be variable names) are rejected.
const RESERVED: &[&str] = &[
    "TIME",
    "RENDERSIZE",
    "PASSINDEX",
    "FRAMEINDEX",
    "TIMEDELTA",
    "DATE",
    "IMG_PIXEL",
    "IMG_NORM_PIXEL",
    "IMG_THIS_PIXEL",
    "IMG_SIZE",
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
];

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
            }
            .map(finite);
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
        s.push_str(&format!("#define vjc_u_{} {}\n", i.name, expr));
    }
    let names: Vec<&str> = isf.inputs.iter().map(|i| i.name.as_str()).collect();
    s.push_str(&rename_inputs(&isf.body, &names));
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

    #[test]
    fn all_generators_compile() {
        assert_eq!(GENERATORS.len(), 6);
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
}
