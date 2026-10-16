/*{
  "DESCRIPTION": "Abstract field: a slowly flowing, domain-warped colour field.",
  "CATEGORIES": ["Generator"],
  "INPUTS": [
    { "NAME": "tint_a", "TYPE": "color", "DEFAULT": [0.1, 0.2, 0.9, 1.0] },
    { "NAME": "tint_b", "TYPE": "color", "DEFAULT": [1.0, 0.3, 0.5, 1.0] },
    { "NAME": "scale", "TYPE": "float", "MIN": 0.5, "MAX": 8.0, "DEFAULT": 2.5 },
    { "NAME": "warp", "TYPE": "float", "MIN": 0.0, "MAX": 3.0, "DEFAULT": 1.2 },
    { "NAME": "speed", "TYPE": "float", "MIN": 0.0, "MAX": 3.0, "DEFAULT": 0.4 },
    { "NAME": "contrast", "TYPE": "float", "MIN": 0.2, "MAX": 3.0, "DEFAULT": 1.2 }
  ]
}*/
void main() {
    float aspect = RENDERSIZE.x / RENDERSIZE.y;
    vec2 p = (isf_FragNormCoord - 0.5) * vec2(aspect, 1.0) * scale;
    float t = TIME * speed;
    for (int i = 0; i < 4; i++) {
        float fi = float(i + 1);
        p += warp * 0.5 / fi * vec2(sin(p.y * fi + t * 1.3 + fi), cos(p.x * fi - t + fi * 2.0));
    }
    float v = 0.5 + 0.5 * sin(p.x * 1.7 + p.y * 1.1 + t);
    v = clamp((v - 0.5) * contrast + 0.5, 0.0, 1.0);
    float glow = 0.5 + 0.5 * sin(length(p) * 2.0 - t * 1.5);
    vec3 col = mix(tint_a.rgb, tint_b.rgb, v) * (0.55 + 0.45 * glow);
    gl_FragColor = vec4(col, 1.0);
}
