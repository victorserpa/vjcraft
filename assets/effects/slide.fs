/*{
  "DESCRIPTION": "Scrolls the picture with wrap-around, smoothly or in beat-synced steps.",
  "CATEGORIES": ["Geometry"],
  "INPUTS": [
    { "NAME": "inputImage", "TYPE": "image" },
    { "NAME": "bpm", "TYPE": "float", "MIN": 40.0, "MAX": 220.0, "DEFAULT": 120.0 },
    { "NAME": "dir_x", "TYPE": "float", "MIN": -1.0, "MAX": 1.0, "DEFAULT": 1.0 },
    { "NAME": "dir_y", "TYPE": "float", "MIN": -1.0, "MAX": 1.0, "DEFAULT": 0.0 },
    { "NAME": "distance_per_beat", "TYPE": "float", "MIN": 0.0, "MAX": 1.0, "DEFAULT": 0.25 },
    { "NAME": "snap", "TYPE": "bool", "DEFAULT": true }
  ]
}*/
void main() {
    float beat = TIME * bpm / 60.0;
    float pos = beat;
    if (snap) {
        float f = fract(beat);
        float e = f * f * (3.0 - 2.0 * f);
        pos = floor(beat) + smoothstep(0.0, 1.0, e * 1.0);
    }
    vec2 uv = fract(isf_FragNormCoord - vec2(dir_x, dir_y) * distance_per_beat * pos);
    gl_FragColor = IMG_NORM_PIXEL(inputImage, uv);
}
