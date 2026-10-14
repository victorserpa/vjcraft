/*{
  "DESCRIPTION": "Worn tape look: line wobble, colour bleed, tracking band and static.",
  "CATEGORIES": ["Stylize"],
  "INPUTS": [
    { "NAME": "inputImage", "TYPE": "image" },
    { "NAME": "wobble", "TYPE": "float", "MIN": 0.0, "MAX": 0.05, "DEFAULT": 0.004 },
    { "NAME": "bleed", "TYPE": "float", "MIN": 0.0, "MAX": 0.05, "DEFAULT": 0.006 },
    { "NAME": "static_amount", "TYPE": "float", "MIN": 0.0, "MAX": 1.0, "DEFAULT": 0.12 },
    { "NAME": "tracking", "TYPE": "float", "MIN": 0.0, "MAX": 1.0, "DEFAULT": 0.4 }
  ]
}*/
float hash(vec2 p) {
    return fract(sin(dot(p, vec2(127.1, 311.7))) * 43758.5453);
}
void main() {
    vec2 uv = isf_FragNormCoord;
    float row = floor(uv.y * RENDERSIZE.y);
    float tick = floor(TIME * 25.0);
    float band = (1.0 - smoothstep(0.0, 0.03, abs(fract(uv.y * 0.5 - TIME * 0.2) - 0.5) - 0.45)) * tracking;
    float jx = (hash(vec2(row, tick)) - 0.5) * wobble * (1.0 + 6.0 * band);
    uv.x += jx;
    float r = IMG_NORM_PIXEL(inputImage, uv + vec2(bleed, 0.0)).r;
    vec4 g = IMG_NORM_PIXEL(inputImage, uv);
    float b = IMG_NORM_PIXEL(inputImage, uv - vec2(bleed, 0.0)).b;
    float n = hash(vec2(floor(isf_FragNormCoord.x * RENDERSIZE.x), row) + vec2(tick * 7.0, tick));
    vec3 col = vec3(r, g.g, b) + (n - 0.5) * static_amount * (1.0 + 3.0 * band);
    gl_FragColor = vec4(clamp(col, 0.0, 1.0), g.a);
}
