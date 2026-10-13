/*{
  "DESCRIPTION": "Two-color circular gradient.",
  "CATEGORIES": ["Generator"],
  "INPUTS": [
    { "NAME": "inner", "TYPE": "color", "DEFAULT": [1.0, 1.0, 1.0, 1.0] },
    { "NAME": "outer", "TYPE": "color", "DEFAULT": [0.0, 0.0, 0.0, 1.0] },
    { "NAME": "center", "TYPE": "point2D", "MIN": [0.0, 0.0], "MAX": [1.0, 1.0], "DEFAULT": [0.5, 0.5] },
    { "NAME": "radius", "TYPE": "float", "MIN": 0.01, "MAX": 2.0, "DEFAULT": 0.5 }
  ]
}*/
void main() {
    vec2 p = (isf_FragNormCoord - center) * vec2(RENDERSIZE.x / RENDERSIZE.y, 1.0);
    float t = clamp(length(p) / max(radius, 0.001), 0.0, 1.0);
    gl_FragColor = mix(inner, outer, t);
}
