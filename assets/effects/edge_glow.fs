/*{
  "DESCRIPTION": "Finds edges and makes them glow in a colour on top of the picture.",
  "CATEGORIES": ["Stylize"],
  "INPUTS": [
    { "NAME": "inputImage", "TYPE": "image" },
    { "NAME": "glow_color", "TYPE": "color", "DEFAULT": [0.2, 0.9, 1.0, 1.0] },
    { "NAME": "gain", "TYPE": "float", "MIN": 0.0, "MAX": 8.0, "DEFAULT": 3.0 },
    { "NAME": "spread", "TYPE": "float", "MIN": 1.0, "MAX": 12.0, "DEFAULT": 3.0 },
    { "NAME": "base", "TYPE": "float", "MIN": 0.0, "MAX": 1.0, "DEFAULT": 0.6 }
  ]
}*/
float lum_(vec2 uv) {
    return dot(IMG_NORM_PIXEL(inputImage, uv).rgb, vec3(0.299, 0.587, 0.114));
}
float sobel_(vec2 uv, vec2 px) {
    float tl = lum_(uv + px * vec2(-1.0, 1.0));
    float t = lum_(uv + px * vec2(0.0, 1.0));
    float tr = lum_(uv + px * vec2(1.0, 1.0));
    float l = lum_(uv + px * vec2(-1.0, 0.0));
    float r = lum_(uv + px * vec2(1.0, 0.0));
    float bl = lum_(uv + px * vec2(-1.0, -1.0));
    float b = lum_(uv + px * vec2(0.0, -1.0));
    float br = lum_(uv + px * vec2(1.0, -1.0));
    float gx = -tl - 2.0 * l - bl + tr + 2.0 * r + br;
    float gy = -tl - 2.0 * t - tr + bl + 2.0 * b + br;
    return length(vec2(gx, gy));
}
void main() {
    vec2 uv = isf_FragNormCoord;
    vec2 px = 1.0 / RENDERSIZE;
    float e = sobel_(uv, px) * 0.6 + sobel_(uv, px * spread) * 0.4;
    e = clamp(e * gain, 0.0, 1.0);
    vec4 c = IMG_THIS_PIXEL(inputImage);
    gl_FragColor = vec4(clamp(c.rgb * base + glow_color.rgb * e, 0.0, 1.0), c.a);
}
