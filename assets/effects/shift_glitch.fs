/*{
  "DESCRIPTION": "Slides random horizontal and vertical blocks sideways on the beat, with a colour split.",
  "CATEGORIES": ["Stylize"],
  "INPUTS": [
    { "NAME": "inputImage", "TYPE": "image" },
    { "NAME": "bpm", "TYPE": "float", "MIN": 40.0, "MAX": 220.0, "DEFAULT": 120.0 },
    { "NAME": "blocks", "TYPE": "float", "MIN": 2.0, "MAX": 60.0, "DEFAULT": 14.0 },
    { "NAME": "shift", "TYPE": "float", "MIN": 0.0, "MAX": 0.5, "DEFAULT": 0.12 },
    { "NAME": "density", "TYPE": "float", "MIN": 0.0, "MAX": 1.0, "DEFAULT": 0.4 },
    { "NAME": "split", "TYPE": "float", "MIN": 0.0, "MAX": 0.05, "DEFAULT": 0.008 }
  ]
}*/
float h2_(vec2 p) {
    return fract(sin(dot(p, vec2(127.1, 311.7))) * 43758.5453);
}
void main() {
    vec2 uv = isf_FragNormCoord;
    float beat = TIME * bpm / 60.0;
    // Glitch frames last the first part of every beat; the rest is clean.
    float live = 1.0 - step(0.45, fract(beat));
    float roll = floor(beat * 2.0);
    float row = floor(uv.y * blocks);
    float hit = step(1.0 - density, h2_(vec2(row, roll)));
    float dx = (h2_(vec2(row, roll + 9.0)) - 0.5) * 2.0 * shift * hit * live;
    float col = floor(uv.x * blocks);
    float hit_v = step(1.0 - density * 0.5, h2_(vec2(col, roll + 21.0)));
    float dy = (h2_(vec2(col, roll + 33.0)) - 0.5) * 2.0 * shift * hit_v * live;
    vec2 q = uv + vec2(dx, dy);
    float s = split * hit * live;
    float r = IMG_NORM_PIXEL(inputImage, q + vec2(s, 0.0)).r;
    vec4 g = IMG_NORM_PIXEL(inputImage, q);
    float b = IMG_NORM_PIXEL(inputImage, q - vec2(s, 0.0)).b;
    gl_FragColor = vec4(r, g.g, b, g.a);
}
