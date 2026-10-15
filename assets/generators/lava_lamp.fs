/*{
  "DESCRIPTION": "Warm blobs of wax that rise, sink and merge like a lava lamp.",
  "CATEGORIES": ["Generator"],
  "INPUTS": [
    { "NAME": "wax", "TYPE": "color", "DEFAULT": [1.0, 0.3, 0.1, 1.0] },
    { "NAME": "waxHot", "TYPE": "color", "DEFAULT": [1.0, 0.85, 0.2, 1.0] },
    { "NAME": "liquid", "TYPE": "color", "DEFAULT": [0.25, 0.02, 0.35, 1.0] },
    { "NAME": "speed", "TYPE": "float", "MIN": 0.0, "MAX": 3.0, "DEFAULT": 0.5 },
    { "NAME": "blobSize", "TYPE": "float", "MIN": 0.05, "MAX": 0.3, "DEFAULT": 0.12 },
    { "NAME": "goo", "TYPE": "float", "MIN": 0.5, "MAX": 3.0, "DEFAULT": 1.5 }
  ]
}*/
void main() {
    float aspect = RENDERSIZE.x / RENDERSIZE.y;
    vec2 uv = isf_FragNormCoord;
    vec2 p = uv * vec2(aspect, 1.0);
    float t = TIME * speed;
    float field = 0.0;
    for (int i = 0; i < 7; i++) {
        float fi = float(i);
        float rise = 0.5 + 0.5 * sin(t * (0.45 + 0.13 * fi) + fi * 2.3);
        vec2 c = vec2(aspect * (0.5 + 0.28 * sin(t * 0.3 + fi * 1.7)),
                      0.08 + 0.84 * rise);
        float r = blobSize * (0.7 + 0.5 * fract(fi * 0.618));
        vec2 d = p - c;
        d.y *= 0.8;
        field += r * r / (dot(d, d) + 1e-4);
    }
    float m = smoothstep(1.0, 1.0 + 0.4 / goo, field);
    float heat = 1.0 - uv.y;
    vec3 bg = liquid.rgb * (0.6 + 0.8 * heat) + wax.rgb * 0.08 * heat;
    vec3 blob = mix(wax.rgb, waxHot.rgb, clamp(heat * 0.8 + (field - 1.0) * 0.15, 0.0, 1.0));
    gl_FragColor = vec4(mix(bg, blob, m), 1.0);
}
