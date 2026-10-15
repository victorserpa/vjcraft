/*{
  "DESCRIPTION": "Oscilloscope-style waveform line. ISF audio inputs are not supported here, so the wave is synthetic (a few sines that swell on the beat).",
  "CATEGORIES": ["Generator"],
  "INPUTS": [
    { "NAME": "lineColor", "TYPE": "color", "DEFAULT": [0.3, 1.0, 0.6, 1.0] },
    { "NAME": "background", "TYPE": "color", "DEFAULT": [0.0, 0.03, 0.04, 1.0] },
    { "NAME": "amplitude", "TYPE": "float", "MIN": 0.05, "MAX": 0.5, "DEFAULT": 0.25 },
    { "NAME": "thickness", "TYPE": "float", "MIN": 0.002, "MAX": 0.05, "DEFAULT": 0.008 },
    { "NAME": "glow", "TYPE": "float", "MIN": 0.0, "MAX": 3.0, "DEFAULT": 1.0 },
    { "NAME": "bpm", "TYPE": "float", "MIN": 40.0, "MAX": 220.0, "DEFAULT": 120.0 }
  ]
}*/
float wave_(float x) {
    float kick = exp(-fract(TIME * bpm / 60.0) * 4.0);
    float env = 0.5 + 0.5 * kick;
    float w = sin(x * 9.0 + TIME * 3.0) * 0.5
            + sin(x * 23.0 - TIME * 5.0) * 0.25 * env
            + sin(x * 51.0 + TIME * 9.0) * 0.12;
    return w * sin(x * 3.1416) * env;
}
void main() {
    vec2 uv = isf_FragNormCoord;
    float y = 0.5 + wave_(uv.x) * amplitude * 2.0;
    // Distance to the curve, corrected for slope so steep sections stay the same width.
    float e = 0.002;
    float slope = (wave_(uv.x + e) - wave_(uv.x - e)) / (2.0 * e) * amplitude * 2.0;
    float d = abs(uv.y - y) / sqrt(1.0 + slope * slope);
    float core = 1.0 - smoothstep(thickness, thickness * 1.8, d);
    float halo = glow * thickness * 3.0 / (d + thickness * 3.0);
    vec3 col = background.rgb + lineColor.rgb * (core + halo * halo * 0.6);
    gl_FragColor = vec4(clamp(col, 0.0, 1.0), 1.0);
}
