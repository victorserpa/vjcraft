/*{
  "DESCRIPTION": "Sobel outline of brightness changes.",
  "CATEGORIES": ["Stylize"],
  "INPUTS": [
    { "NAME": "inputImage", "TYPE": "image" },
    { "NAME": "strength", "TYPE": "float", "MIN": 0.0, "MAX": 8.0, "DEFAULT": 2.0 },
    { "NAME": "keep_color", "TYPE": "float", "MIN": 0.0, "MAX": 1.0, "DEFAULT": 0.0 },
    { "NAME": "line", "TYPE": "color", "DEFAULT": [1.0, 1.0, 1.0, 1.0] }
  ]
}*/
float lum(vec2 uv) {
    return dot(IMG_NORM_PIXEL(inputImage, uv).rgb, vec3(0.2126, 0.7152, 0.0722));
}
void main() {
    vec2 px = vec2(1.0) / RENDERSIZE;
    vec2 u = isf_FragNormCoord;
    float tl = lum(u + px * vec2(-1.0, 1.0));
    float t = lum(u + px * vec2(0.0, 1.0));
    float tr = lum(u + px * vec2(1.0, 1.0));
    float l = lum(u + px * vec2(-1.0, 0.0));
    float r = lum(u + px * vec2(1.0, 0.0));
    float bl = lum(u + px * vec2(-1.0, -1.0));
    float b = lum(u + px * vec2(0.0, -1.0));
    float br = lum(u + px * vec2(1.0, -1.0));
    float gx = -tl - 2.0 * l - bl + tr + 2.0 * r + br;
    float gy = -tl - 2.0 * t - tr + bl + 2.0 * b + br;
    float e = clamp(length(vec2(gx, gy)) * strength, 0.0, 1.0);
    vec4 c = IMG_THIS_PIXEL(inputImage);
    vec3 base = c.rgb * keep_color;
    gl_FragColor = vec4(mix(base, line.rgb, e), c.a);
}
