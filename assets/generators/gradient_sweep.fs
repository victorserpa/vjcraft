/*{
  "DESCRIPTION": "Rotating conic (angular) gradient between three colours.",
  "CATEGORIES": ["Generator"],
  "INPUTS": [
    { "NAME": "colorA", "TYPE": "color", "DEFAULT": [0.95, 0.2, 0.4, 1.0] },
    { "NAME": "colorB", "TYPE": "color", "DEFAULT": [0.2, 0.3, 0.95, 1.0] },
    { "NAME": "colorC", "TYPE": "color", "DEFAULT": [0.2, 0.95, 0.6, 1.0] },
    { "NAME": "speed", "TYPE": "float", "MIN": -2.0, "MAX": 2.0, "DEFAULT": 0.2 },
    { "NAME": "repeat", "TYPE": "float", "MIN": 1.0, "MAX": 8.0, "DEFAULT": 1.0 },
    { "NAME": "center", "TYPE": "point2D", "DEFAULT": [0.5, 0.5] }
  ]
}*/
void main() {
    float aspect = RENDERSIZE.x / RENDERSIZE.y;
    vec2 d = (isf_FragNormCoord - center) * vec2(aspect, 1.0);
    float t = fract(atan(d.y, d.x) / 6.2831853 * floor(repeat) + TIME * speed) * 3.0;
    vec3 c = t < 1.0 ? mix(colorA.rgb, colorB.rgb, t)
           : t < 2.0 ? mix(colorB.rgb, colorC.rgb, t - 1.0)
                     : mix(colorC.rgb, colorA.rgb, t - 2.0);
    gl_FragColor = vec4(c, 1.0);
}
