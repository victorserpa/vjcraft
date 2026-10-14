/*{
  "DESCRIPTION": "Concentric rings that expand from a centre point.",
  "CATEGORIES": ["Generator"],
  "INPUTS": [
    { "NAME": "colorA", "TYPE": "color", "DEFAULT": [0.02, 0.02, 0.08, 1.0] },
    { "NAME": "colorB", "TYPE": "color", "DEFAULT": [0.3, 0.8, 1.0, 1.0] },
    { "NAME": "count", "TYPE": "float", "MIN": 1.0, "MAX": 40.0, "DEFAULT": 8.0 },
    { "NAME": "thickness", "TYPE": "float", "MIN": 0.02, "MAX": 0.98, "DEFAULT": 0.4 },
    { "NAME": "speed", "TYPE": "float", "MIN": -4.0, "MAX": 4.0, "DEFAULT": 0.5 },
    { "NAME": "center", "TYPE": "point2D", "MIN": [0.0, 0.0], "MAX": [1.0, 1.0], "DEFAULT": [0.5, 0.5] }
  ]
}*/
void main() {
    float aspect = RENDERSIZE.x / RENDERSIZE.y;
    vec2 p = (isf_FragNormCoord - center) * vec2(aspect, 1.0);
    float f = fract(length(p) * count - TIME * speed);
    float edge = 0.02 + 0.5 * fwidth(f);
    float m = smoothstep(0.0, edge, f) * (1.0 - smoothstep(thickness, thickness + edge, f));
    gl_FragColor = mix(colorA, colorB, m);
}
