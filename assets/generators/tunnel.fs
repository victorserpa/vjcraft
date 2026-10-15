/*{
  "DESCRIPTION": "Fly through a ringed, striped tunnel.",
  "CATEGORIES": ["Generator"],
  "INPUTS": [
    { "NAME": "colorA", "TYPE": "color", "DEFAULT": [0.05, 0.0, 0.2, 1.0] },
    { "NAME": "colorB", "TYPE": "color", "DEFAULT": [0.1, 0.9, 1.0, 1.0] },
    { "NAME": "speed", "TYPE": "float", "MIN": -4.0, "MAX": 4.0, "DEFAULT": 0.8 },
    { "NAME": "segments", "TYPE": "float", "MIN": 2.0, "MAX": 32.0, "DEFAULT": 8.0 },
    { "NAME": "twist", "TYPE": "float", "MIN": -2.0, "MAX": 2.0, "DEFAULT": 0.3 }
  ]
}*/
void main() {
    float aspect = RENDERSIZE.x / RENDERSIZE.y;
    vec2 p = (isf_FragNormCoord - 0.5) * vec2(aspect, 1.0);
    float r = max(length(p), 1e-3);
    float depth = 0.3 / r;
    float a = atan(p.y, p.x) / 6.2831853;
    float u = a * segments + depth * twist;
    float v = depth + TIME * speed;
    float k = mod(step(0.5, fract(u)) + step(0.5, fract(v)), 2.0) * 0.75 + 0.25;
    vec3 c = mix(colorA.rgb, colorB.rgb, k);
    c *= 0.35 + 0.65 * smoothstep(0.0, 0.25, r);
    gl_FragColor = vec4(max(c, vec3(0.02)), 1.0);
}
