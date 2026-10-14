/*{
  "DESCRIPTION": "Classic sine-interference plasma blended through a three-colour palette.",
  "CATEGORIES": ["Generator"],
  "INPUTS": [
    { "NAME": "colorA", "TYPE": "color", "DEFAULT": [0.1, 0.0, 0.35, 1.0] },
    { "NAME": "colorB", "TYPE": "color", "DEFAULT": [0.95, 0.2, 0.5, 1.0] },
    { "NAME": "colorC", "TYPE": "color", "DEFAULT": [1.0, 0.85, 0.3, 1.0] },
    { "NAME": "scale", "TYPE": "float", "MIN": 0.5, "MAX": 20.0, "DEFAULT": 4.0 },
    { "NAME": "speed", "TYPE": "float", "MIN": 0.0, "MAX": 4.0, "DEFAULT": 0.6 }
  ]
}*/
void main() {
    float aspect = RENDERSIZE.x / RENDERSIZE.y;
    vec2 p = (isf_FragNormCoord - 0.5) * vec2(aspect, 1.0) * scale;
    float t = TIME * speed;
    float v = sin(p.x * 1.7 + t)
        + sin(p.y * 2.1 - t * 1.3)
        + sin((p.x + p.y) * 1.3 + t * 0.7)
        + sin(length(p) * 2.5 - t);
    v = v * 0.125 + 0.5;
    vec3 c = mix(colorA.rgb, colorB.rgb, smoothstep(0.0, 0.5, v));
    c = mix(c, colorC.rgb, smoothstep(0.5, 1.0, v));
    gl_FragColor = vec4(c, 1.0);
}
