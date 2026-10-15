/*{
  "DESCRIPTION": "Draws a coloured outline around the shape found by alpha or brightness.",
  "CATEGORIES": ["Stylize"],
  "INPUTS": [
    { "NAME": "inputImage", "TYPE": "image" },
    { "NAME": "line_color", "TYPE": "color", "DEFAULT": [1.0, 0.85, 0.1, 1.0] },
    { "NAME": "width", "TYPE": "float", "MIN": 0.5, "MAX": 20.0, "DEFAULT": 3.0 },
    { "NAME": "threshold", "TYPE": "float", "MIN": 0.0, "MAX": 1.0, "DEFAULT": 0.5 },
    { "NAME": "from_luma", "TYPE": "bool", "DEFAULT": true }
  ]
}*/
float mask_(vec2 uv) {
    vec4 c = IMG_NORM_PIXEL(inputImage, uv);
    float v = from_luma ? dot(c.rgb, vec3(0.299, 0.587, 0.114)) : c.a;
    return smoothstep(threshold - 0.05, threshold + 0.05, v);
}
void main() {
    vec2 uv = isf_FragNormCoord;
    vec2 px = width / RENDERSIZE;
    float m = mask_(uv);
    float lo = m;
    float hi = m;
    for (int i = 0; i < 8; i++) {
        float a = 0.7853982 * float(i);
        float s = mask_(uv + vec2(cos(a), sin(a)) * px);
        lo = min(lo, s);
        hi = max(hi, s);
    }
    float edge = clamp(hi - lo, 0.0, 1.0) * line_color.a;
    vec4 c = IMG_THIS_PIXEL(inputImage);
    gl_FragColor = vec4(mix(c.rgb, line_color.rgb, edge), max(c.a, edge));
}
