/*{
  "DESCRIPTION": "Checkerboard with optional horizontal scroll.",
  "CATEGORIES": ["Generator"],
  "INPUTS": [
    { "NAME": "colorA", "TYPE": "color", "DEFAULT": [0.0, 0.0, 0.0, 1.0] },
    { "NAME": "colorB", "TYPE": "color", "DEFAULT": [1.0, 1.0, 1.0, 1.0] },
    { "NAME": "tiles", "TYPE": "float", "MIN": 1.0, "MAX": 64.0, "DEFAULT": 8.0 },
    { "NAME": "speed", "TYPE": "float", "MIN": -4.0, "MAX": 4.0, "DEFAULT": 0.0 }
  ]
}*/
void main() {
    vec2 p = isf_FragNormCoord * vec2(tiles * RENDERSIZE.x / RENDERSIZE.y, tiles)
        + vec2(TIME * speed, 0.0);
    vec2 f = floor(p);
    float m = mod(f.x + f.y, 2.0);
    gl_FragColor = mix(colorA, colorB, m);
}
