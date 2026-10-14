/*{
  "DESCRIPTION": "Square tiles of flat colour separated by grout lines.",
  "CATEGORIES": ["Stylize"],
  "INPUTS": [
    { "NAME": "inputImage", "TYPE": "image" },
    { "NAME": "tiles", "TYPE": "float", "MIN": 4.0, "MAX": 160.0, "DEFAULT": 32.0 },
    { "NAME": "grout", "TYPE": "float", "MIN": 0.0, "MAX": 0.5, "DEFAULT": 0.08 },
    { "NAME": "grout_color", "TYPE": "color", "DEFAULT": [0.05, 0.05, 0.05, 1.0] }
  ]
}*/
void main() {
    float aspect = RENDERSIZE.x / RENDERSIZE.y;
    vec2 g = isf_FragNormCoord * vec2(tiles * aspect, tiles);
    vec2 cell = floor(g);
    vec2 f = fract(g) - 0.5;
    vec2 q = (cell + vec2(0.5)) / vec2(tiles * aspect, tiles);
    vec4 c = IMG_NORM_PIXEL(inputImage, q);
    float edge = step(0.5 - grout, max(abs(f.x), abs(f.y)));
    gl_FragColor = vec4(mix(c.rgb, grout_color.rgb, edge), c.a);
}
