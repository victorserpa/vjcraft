/*{
  "DESCRIPTION": "Two-pass separable blur: horizontal pass into a buffer, vertical pass on top of it.",
  "CATEGORIES": ["Blur"],
  "INPUTS": [
    { "NAME": "inputImage", "TYPE": "image" },
    { "NAME": "radius", "TYPE": "float", "MIN": 0.0, "MAX": 32.0, "DEFAULT": 8.0 }
  ],
  "PASSES": [
    { "TARGET": "blurH" },
    {}
  ]
}*/
void main() {
    // 9 taps spread over `radius` pixels each side; pass 0 blurs x, pass 1 blurs y.
    vec2 dir = PASSINDEX == 0 ? vec2(1.0, 0.0) : vec2(0.0, 1.0);
    vec2 step = dir / RENDERSIZE * radius / 4.0;
    vec4 sum = vec4(0.0);
    for (int i = -4; i <= 4; i++) {
        vec2 uv = isf_FragNormCoord + float(i) * step;
        sum += PASSINDEX == 0 ? IMG_NORM_PIXEL(inputImage, uv) : IMG_NORM_PIXEL(blurH, uv);
    }
    gl_FragColor = sum / 9.0;
}
