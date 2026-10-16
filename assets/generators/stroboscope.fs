/*{
  "DESCRIPTION": "Stroboscope: a flat colour that flashes at a steady rate; the off level keeps a faint glow so the source is never fully black.",
  "CATEGORIES": ["Generator"],
  "INPUTS": [
    { "NAME": "flash", "TYPE": "color", "DEFAULT": [1.0, 1.0, 1.0, 1.0] },
    { "NAME": "rate", "TYPE": "float", "MIN": 0.5, "MAX": 25.0, "DEFAULT": 8.0 },
    { "NAME": "duty", "TYPE": "float", "MIN": 0.02, "MAX": 0.9, "DEFAULT": 0.25 },
    { "NAME": "off_level", "TYPE": "float", "MIN": 0.02, "MAX": 0.5, "DEFAULT": 0.06 }
  ]
}*/
void main() {
    float on = step(fract(TIME * rate), duty);
    gl_FragColor = vec4(flash.rgb * mix(off_level, 1.0, on), 1.0);
}
