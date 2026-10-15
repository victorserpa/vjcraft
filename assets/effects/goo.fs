/*{
  "DESCRIPTION": "Slow noise warp that makes the picture wobble like goo; raise smear to leave a melting trail.",
  "CATEGORIES": ["Distort"],
  "PERSISTENT": true,
  "INPUTS": [
    { "NAME": "inputImage", "TYPE": "image" },
    { "NAME": "amount", "TYPE": "float", "MIN": 0.0, "MAX": 0.2, "DEFAULT": 0.04 },
    { "NAME": "scale", "TYPE": "float", "MIN": 0.5, "MAX": 12.0, "DEFAULT": 3.0 },
    { "NAME": "speed", "TYPE": "float", "MIN": 0.0, "MAX": 3.0, "DEFAULT": 0.4 },
    { "NAME": "smear", "TYPE": "float", "MIN": 0.0, "MAX": 0.95, "DEFAULT": 0.0 }
  ]
}*/
float h2_(vec2 p) {
    return fract(sin(dot(p, vec2(127.1, 311.7))) * 43758.5453);
}
float vn_(vec2 p) {
    vec2 i = floor(p);
    vec2 f = fract(p);
    f = f * f * (3.0 - 2.0 * f);
    return mix(mix(h2_(i), h2_(i + vec2(1.0, 0.0)), f.x),
               mix(h2_(i + vec2(0.0, 1.0)), h2_(i + vec2(1.0, 1.0)), f.x), f.y);
}
void main() {
    vec2 uv = isf_FragNormCoord;
    float t = TIME * speed;
    vec2 q = uv * scale;
    vec2 w = vec2(vn_(q + vec2(t, 0.0)) + 0.5 * vn_(q * 2.1 - vec2(0.0, t)),
                  vn_(q + vec2(5.2, 1.3 + t)) + 0.5 * vn_(q * 2.1 + vec2(t, 3.7))) - 0.75;
    vec4 now = IMG_NORM_PIXEL(inputImage, uv + w * amount * 2.0);
    vec4 old = PREV_PIXEL(uv + w * amount * 0.5);
    gl_FragColor = mix(now, old, smear);
}
