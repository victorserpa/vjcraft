/*{
  "DESCRIPTION": "Two-color gradient along an angle.",
  "CATEGORIES": ["Generator"],
  "INPUTS": [
    { "NAME": "colorA", "TYPE": "color", "DEFAULT": [0.0, 0.0, 0.0, 1.0] },
    { "NAME": "colorB", "TYPE": "color", "DEFAULT": [1.0, 1.0, 1.0, 1.0] },
    { "NAME": "angle", "TYPE": "float", "MIN": 0.0, "MAX": 360.0, "DEFAULT": 0.0 }
  ]
}*/
void main() {
    float a = radians(angle);
    vec2 d = vec2(cos(a), sin(a));
    float t = clamp(dot(isf_FragNormCoord - vec2(0.5), d) + 0.5, 0.0, 1.0);
    gl_FragColor = mix(colorA, colorB, t);
}
