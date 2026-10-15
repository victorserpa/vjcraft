/*{
  "DESCRIPTION": "Splits the picture into tiles and shoves each one by a random offset, re-rolled on the beat.",
  "CATEGORIES": ["Distort"],
  "INPUTS": [
    { "NAME": "inputImage", "TYPE": "image" },
    { "NAME": "bpm", "TYPE": "float", "MIN": 40.0, "MAX": 220.0, "DEFAULT": 120.0 },
    { "NAME": "tiles", "TYPE": "float", "MIN": 2.0, "MAX": 40.0, "DEFAULT": 8.0 },
    { "NAME": "amount", "TYPE": "float", "MIN": 0.0, "MAX": 1.0, "DEFAULT": 0.5 },
    { "NAME": "beats_per_roll", "TYPE": "float", "MIN": 0.25, "MAX": 16.0, "DEFAULT": 1.0 }
  ]
}*/
float h2_(vec2 p) {
    return fract(sin(dot(p, vec2(127.1, 311.7))) * 43758.5453);
}
void main() {
    float aspect = RENDERSIZE.x / RENDERSIZE.y;
    vec2 uv = isf_FragNormCoord;
    vec2 g = vec2(tiles * aspect, tiles);
    vec2 id = floor(uv * g);
    float roll = floor(TIME * bpm / 60.0 / beats_per_roll);
    vec2 r = vec2(h2_(id + roll * 7.31), h2_(id.yx + roll * 3.17 + 11.0)) - 0.5;
    // Ease from the old roll to the new one so tiles do not just teleport.
    uv += r * amount / tiles * 4.0;
    gl_FragColor = IMG_NORM_PIXEL(inputImage, uv);
}
