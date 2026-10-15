/*{
  "DESCRIPTION": "Gooey metaballs drifting on Lissajous paths.",
  "CATEGORIES": ["Generator"],
  "INPUTS": [
    { "NAME": "background", "TYPE": "color", "DEFAULT": [0.04, 0.02, 0.12, 1.0] },
    { "NAME": "colorA", "TYPE": "color", "DEFAULT": [1.0, 0.2, 0.5, 1.0] },
    { "NAME": "colorB", "TYPE": "color", "DEFAULT": [1.0, 0.85, 0.3, 1.0] },
    { "NAME": "size", "TYPE": "float", "MIN": 0.02, "MAX": 0.4, "DEFAULT": 0.12 },
    { "NAME": "speed", "TYPE": "float", "MIN": 0.0, "MAX": 4.0, "DEFAULT": 0.7 }
  ]
}*/
void main() {
    float aspect = RENDERSIZE.x / RENDERSIZE.y;
    vec2 p = (isf_FragNormCoord - 0.5) * vec2(aspect, 1.0);
    float t = TIME * speed;
    float field = 0.0;
    for (int i = 0; i < 6; i++) {
        float fi = float(i);
        vec2 c = vec2(sin(t * (0.6 + 0.17 * fi) + fi * 2.1) * 0.5 * aspect * 0.8,
                      cos(t * (0.5 + 0.13 * fi) + fi * 1.3) * 0.38);
        vec2 d = p - c;
        field += size * size / (dot(d, d) + 1e-4);
    }
    float m = smoothstep(0.9, 1.1, field);
    vec3 ball = mix(colorA.rgb, colorB.rgb, clamp((field - 1.0) * 0.25, 0.0, 1.0));
    vec3 glow = colorA.rgb * clamp(field * 0.25, 0.0, 1.0) * 0.5;
    gl_FragColor = vec4(mix(background.rgb + glow, ball, m), 1.0);
}
