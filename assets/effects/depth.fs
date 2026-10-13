/*{
  "DESCRIPTION": "Monocular depth from an ML model (MiDaS small). Needs model-small.onnx, see docs/ml.md.",
  "CATEGORIES": ["ML"],
  "INPUTS": [
    { "NAME": "inputImage", "TYPE": "image" },
    { "NAME": "mode", "TYPE": "long", "VALUES": [0, 1, 2, 3], "LABELS": ["Gray", "Colorize", "Alpha", "Fog"], "DEFAULT": 0 },
    { "NAME": "threshold", "TYPE": "float", "MIN": 0.0, "MAX": 1.0, "DEFAULT": 0.5 },
    { "NAME": "smoothness", "TYPE": "float", "MIN": 0.0, "MAX": 1.0, "DEFAULT": 0.15 },
    { "NAME": "invert", "TYPE": "bool", "DEFAULT": false },
    { "NAME": "fog_color", "TYPE": "color", "DEFAULT": [0.7, 0.75, 0.85, 1.0] }
  ]
}*/
vec3 hsv2rgb(vec3 c) {
    vec3 p = clamp(abs(fract(vec3(c.x) + vec3(0.0, 2.0 / 3.0, 1.0 / 3.0)) * 6.0 - 3.0) - 1.0, 0.0, 1.0);
    return c.z * mix(vec3(1.0), p, c.y);
}
void main() {
    vec4 c = IMG_THIS_PIXEL(inputImage);
    // 1 = near, 0 = far.
    float d = IMG_THIS_PIXEL(vjc_mask).r;
    if (invert) { d = 1.0 - d; }
    float s = max(smoothness * 0.5, 0.0005);
    if (mode == 0) {
        gl_FragColor = vec4(vec3(d), c.a);
    } else if (mode == 1) {
        gl_FragColor = vec4(hsv2rgb(vec3((1.0 - d) * 0.7, 0.9, 1.0)), c.a);
    } else if (mode == 2) {
        gl_FragColor = vec4(c.rgb, c.a * smoothstep(threshold - s, threshold + s, d));
    } else {
        float fog = 1.0 - smoothstep(threshold - s, threshold + s, d);
        gl_FragColor = vec4(mix(c.rgb, fog_color.rgb, fog * fog_color.a), c.a);
    }
}
