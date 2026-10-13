/*{
  "DESCRIPTION": "A bar sweeping across the frame, driven by TIME.",
  "CATEGORIES": ["Generator"],
  "INPUTS": [
    { "NAME": "colorA", "TYPE": "color", "DEFAULT": [0.0, 0.0, 0.0, 1.0] },
    { "NAME": "colorB", "TYPE": "color", "DEFAULT": [1.0, 1.0, 1.0, 1.0] },
    { "NAME": "barWidth", "TYPE": "float", "MIN": 0.01, "MAX": 1.0, "DEFAULT": 0.1 },
    { "NAME": "speed", "TYPE": "float", "MIN": -4.0, "MAX": 4.0, "DEFAULT": 0.5 },
    { "NAME": "vertical", "TYPE": "bool", "DEFAULT": false }
  ]
}*/
void main() {
    float u = vertical ? isf_FragNormCoord.y : isf_FragNormCoord.x;
    float pos = fract(TIME * speed);
    float d = abs(fract(u - pos + 0.5) - 0.5);
    float m = step(d, barWidth * 0.5);
    gl_FragColor = mix(colorA, colorB, m);
}
