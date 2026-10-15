/*{
  "DESCRIPTION": "Pencil sketch: dark edge lines over paper.",
  "CATEGORIES": ["Stylize"],
  "INPUTS": [
    { "NAME": "inputImage", "TYPE": "image" },
    { "NAME": "strength", "TYPE": "float", "MIN": 0.5, "MAX": 10.0, "DEFAULT": 4.0 },
    { "NAME": "paper", "TYPE": "color", "DEFAULT": [0.97, 0.95, 0.9, 1.0] },
    { "NAME": "ink", "TYPE": "color", "DEFAULT": [0.1, 0.1, 0.15, 1.0] }
  ]
}*/
float lum(vec2 uv) {
    return dot(IMG_NORM_PIXEL(inputImage, uv).rgb, vec3(0.299, 0.587, 0.114));
}
void main() {
    vec2 uv = isf_FragNormCoord;
    vec2 px = 1.0 / RENDERSIZE;
    float gx = lum(uv + vec2(px.x, 0.0)) - lum(uv - vec2(px.x, 0.0));
    float gy = lum(uv + vec2(0.0, px.y)) - lum(uv - vec2(0.0, px.y));
    float e = clamp(length(vec2(gx, gy)) * strength, 0.0, 1.0);
    float a = IMG_THIS_PIXEL(inputImage).a;
    gl_FragColor = vec4(mix(paper.rgb, ink.rgb, e), a);
}
