/*{
  "DESCRIPTION": "Rotating spiral arms with adjustable arm count, tightness and direction.",
  "CATEGORIES": ["Generator"],
  "INPUTS": [
    { "NAME": "colorA", "TYPE": "color", "DEFAULT": [0.05, 0.0, 0.15, 1.0] },
    { "NAME": "colorB", "TYPE": "color", "DEFAULT": [1.0, 0.6, 0.1, 1.0] },
    { "NAME": "arms", "TYPE": "float", "MIN": 1.0, "MAX": 16.0, "DEFAULT": 3.0 },
    { "NAME": "tightness", "TYPE": "float", "MIN": -20.0, "MAX": 20.0, "DEFAULT": 6.0 },
    { "NAME": "speed", "TYPE": "float", "MIN": -4.0, "MAX": 4.0, "DEFAULT": 0.8 },
    { "NAME": "sharpness", "TYPE": "float", "MIN": 0.05, "MAX": 1.0, "DEFAULT": 0.5 },
    { "NAME": "center", "TYPE": "point2D", "MIN": [0.0, 0.0], "MAX": [1.0, 1.0], "DEFAULT": [0.5, 0.5] }
  ]
}*/
void main() {
    float aspect = RENDERSIZE.x / RENDERSIZE.y;
    vec2 p = (isf_FragNormCoord - center) * vec2(aspect, 1.0);
    float r = length(p);
    float a = atan(p.y, p.x);
    float u = a / 6.2831853 * floor(arms) + log(r + 0.02) * tightness / 6.2831853 * 2.0 - TIME * speed;
    float f = fract(u);
    float e = 0.5 * fwidth(u) + 0.02;
    float m = smoothstep(0.0, sharpness * 0.5 + e, f) * (1.0 - smoothstep(0.5, 0.5 + sharpness * 0.5 + e, f));
    vec3 col = mix(colorA.rgb, colorB.rgb, m);
    col *= 0.35 + 0.65 * smoothstep(0.0, 0.25, r);
    gl_FragColor = vec4(col + colorB.rgb * 0.05, 1.0);
}
