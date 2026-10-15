/*{
  "DESCRIPTION": "Retro low resolution and reduced colour depth.",
  "CATEGORIES": ["Stylize"],
  "INPUTS": [
    { "NAME": "inputImage", "TYPE": "image" },
    { "NAME": "pixels_across", "TYPE": "float", "MIN": 8.0, "MAX": 640.0, "DEFAULT": 96.0 },
    { "NAME": "levels", "TYPE": "float", "MIN": 2.0, "MAX": 32.0, "DEFAULT": 6.0 },
    { "NAME": "dither", "TYPE": "float", "MIN": 0.0, "MAX": 1.0, "DEFAULT": 0.5 }
  ]
}*/
void main() {
    float aspect = RENDERSIZE.x / RENDERSIZE.y;
    vec2 g = vec2(pixels_across, pixels_across / aspect);
    vec2 cell = floor(isf_FragNormCoord * g);
    vec4 c = IMG_NORM_PIXEL(inputImage, (cell + 0.5) / g);
    // 2x2 ordered dither on the low-res grid.
    vec2 m = mod(cell, 2.0);
    float bayer = (m.x + 2.0 * m.y) * (1.0 / 4.0) + (m.x * m.y > 0.5 ? 0.0 : 0.0);
    float bias = (bayer - 0.375) * dither / levels;
    vec3 q = floor(clamp(c.rgb + bias, 0.0, 1.0) * (levels - 1.0) + 0.5) / (levels - 1.0);
    gl_FragColor = vec4(q, c.a);
}
