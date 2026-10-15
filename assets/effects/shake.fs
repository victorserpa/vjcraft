/*{
  "DESCRIPTION": "Camera shake with a kick on every beat (set bpm to the song tempo).",
  "CATEGORIES": ["Geometry"],
  "INPUTS": [
    { "NAME": "inputImage", "TYPE": "image" },
    { "NAME": "amount", "TYPE": "float", "MIN": 0.0, "MAX": 0.2, "DEFAULT": 0.03 },
    { "NAME": "bpm", "TYPE": "float", "MIN": 40.0, "MAX": 220.0, "DEFAULT": 120.0 },
    { "NAME": "decay", "TYPE": "float", "MIN": 0.5, "MAX": 12.0, "DEFAULT": 4.0 },
    { "NAME": "sync", "TYPE": "bool", "DEFAULT": true }
  ]
}*/
void main() {
    float beat = TIME * bpm / 60.0;
    float env = sync ? exp(-fract(beat) * decay) : 1.0;
    float t = TIME * 40.0;
    vec2 off = vec2(sin(t * 1.3) + sin(t * 2.9 + 1.0), cos(t * 1.7) + sin(t * 3.7 + 2.0)) * 0.5;
    gl_FragColor = IMG_NORM_PIXEL(inputImage, isf_FragNormCoord + off * amount * env);
}
