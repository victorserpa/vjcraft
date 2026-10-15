/*{
  "DESCRIPTION": "Rotates the hue around the colour wheel, locked to the tempo.",
  "CATEGORIES": ["Color"],
  "INPUTS": [
    { "NAME": "inputImage", "TYPE": "image" },
    { "NAME": "bpm", "TYPE": "float", "MIN": 40.0, "MAX": 220.0, "DEFAULT": 120.0 },
    { "NAME": "beats_per_cycle", "TYPE": "float", "MIN": 1.0, "MAX": 64.0, "DEFAULT": 8.0 },
    { "NAME": "offset", "TYPE": "float", "MIN": 0.0, "MAX": 1.0, "DEFAULT": 0.0 }
  ]
}*/
void main() {
    vec4 c = IMG_THIS_PIXEL(inputImage);
    float beat = TIME * bpm / 60.0;
    float a = 6.2831853 * fract(beat / beats_per_cycle + offset);
    float cs = cos(a);
    float sn = sin(a);
    mat3 m = mat3(
        0.299 + 0.701 * cs + 0.168 * sn, 0.299 - 0.299 * cs - 0.328 * sn, 0.299 - 0.3 * cs + 1.25 * sn,
        0.587 - 0.587 * cs + 0.330 * sn, 0.587 + 0.413 * cs + 0.035 * sn, 0.587 - 0.588 * cs - 1.05 * sn,
        0.114 - 0.114 * cs - 0.497 * sn, 0.114 - 0.114 * cs + 0.292 * sn, 0.114 + 0.886 * cs - 0.203 * sn);
    gl_FragColor = vec4(clamp(m * c.rgb, 0.0, 1.0), c.a);
}
