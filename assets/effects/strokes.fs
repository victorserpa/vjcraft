/*{
  "DESCRIPTION": "Strokes: smears the picture along its own contours so it reads like brush strokes.",
  "CATEGORIES": ["Stylize"],
  "INPUTS": [
    { "NAME": "inputImage", "TYPE": "image" },
    { "NAME": "stroke_length", "TYPE": "float", "MIN": 0.0, "MAX": 0.15, "DEFAULT": 0.04 },
    { "NAME": "contrast", "TYPE": "float", "MIN": 0.0, "MAX": 2.0, "DEFAULT": 1.0 },
    { "NAME": "jitter", "TYPE": "float", "MIN": 0.0, "MAX": 1.0, "DEFAULT": 0.4 },
    { "NAME": "mix_original", "TYPE": "float", "MIN": 0.0, "MAX": 1.0, "DEFAULT": 0.0 }
  ]
}*/
float luma_at(vec2 uv) {
    vec3 c = IMG_NORM_PIXEL(inputImage, uv).rgb;
    return dot(c, vec3(0.299, 0.587, 0.114));
}
float hash(vec2 p) {
    return fract(sin(dot(p, vec2(127.1, 311.7))) * 43758.5453);
}
void main() {
    vec2 uv = isf_FragNormCoord;
    float aspect = RENDERSIZE.x / RENDERSIZE.y;
    vec2 e = vec2(1.5) / RENDERSIZE;
    vec2 grad = vec2(luma_at(uv + vec2(e.x, 0.0)) - luma_at(uv - vec2(e.x, 0.0)),
                     luma_at(uv + vec2(0.0, e.y)) - luma_at(uv - vec2(0.0, e.y)));
    // Strokes run along the contour, perpendicular to the gradient; flat areas get a random heading.
    float strength = clamp(length(grad) * 8.0 * contrast, 0.0, 1.0);
    float rnd = hash(floor(uv * RENDERSIZE / 6.0)) * 6.2831853;
    vec2 dir = length(grad) > 1e-4 ? normalize(vec2(-grad.y, grad.x)) : vec2(cos(rnd), sin(rnd));
    vec2 rd = vec2(cos(rnd), sin(rnd));
    dir = normalize(mix(rd, dir, strength) + vec2(1e-5));
    vec2 step_uv = dir * stroke_length / 6.0 * vec2(1.0 / aspect, 1.0);
    float j = (hash(uv * RENDERSIZE) - 0.5) * jitter;
    vec4 sum = vec4(0.0);
    for (int i = -6; i <= 6; i++) {
        sum += IMG_NORM_PIXEL(inputImage, uv + step_uv * (float(i) + j));
    }
    vec4 smear = sum / 13.0;
    gl_FragColor = vec4(mix(smear.rgb, IMG_NORM_PIXEL(inputImage, uv).rgb, mix_original), 1.0);
}
