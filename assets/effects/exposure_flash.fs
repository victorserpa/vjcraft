/*{
  "DESCRIPTION": "Flashes to white on every beat and decays back to the picture.",
  "CATEGORIES": ["Time"],
  "INPUTS": [
    { "NAME": "inputImage", "TYPE": "image" },
    { "NAME": "bpm", "TYPE": "float", "MIN": 40.0, "MAX": 220.0, "DEFAULT": 120.0 },
    { "NAME": "beats_per_flash", "TYPE": "float", "MIN": 0.25, "MAX": 16.0, "DEFAULT": 1.0 },
    { "NAME": "decay", "TYPE": "float", "MIN": 0.5, "MAX": 20.0, "DEFAULT": 5.0 },
    { "NAME": "intensity", "TYPE": "float", "MIN": 0.0, "MAX": 1.0, "DEFAULT": 0.8 }
  ]
}*/
void main() {
    vec4 c = IMG_THIS_PIXEL(inputImage);
    float ph = fract(TIME * bpm / 60.0 / beats_per_flash);
    float f = exp(-ph * decay) * intensity;
    gl_FragColor = vec4(mix(c.rgb, vec3(1.0), f), c.a);
}
