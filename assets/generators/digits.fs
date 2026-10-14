/*{
  "DESCRIPTION": "Four-digit seven-segment counter that counts up with time.",
  "CATEGORIES": ["Generator"],
  "INPUTS": [
    { "NAME": "background", "TYPE": "color", "DEFAULT": [0.03, 0.03, 0.05, 1.0] },
    { "NAME": "lit", "TYPE": "color", "DEFAULT": [1.0, 0.3, 0.2, 1.0] },
    { "NAME": "unlit", "TYPE": "color", "DEFAULT": [0.14, 0.05, 0.05, 1.0] },
    { "NAME": "rate", "TYPE": "float", "MIN": 0.0, "MAX": 100.0, "DEFAULT": 1.0 },
    { "NAME": "size", "TYPE": "float", "MIN": 0.2, "MAX": 1.0, "DEFAULT": 0.7 },
    { "NAME": "thickness", "TYPE": "float", "MIN": 0.05, "MAX": 0.3, "DEFAULT": 0.12 }
  ]
}*/
int glyph(int d) {
    if (d == 0) { return 63; }
    if (d == 1) { return 6; }
    if (d == 2) { return 91; }
    if (d == 3) { return 79; }
    if (d == 4) { return 102; }
    if (d == 5) { return 109; }
    if (d == 6) { return 125; }
    if (d == 7) { return 7; }
    if (d == 8) { return 127; }
    return 111;
}
float bar(vec2 p, vec2 c, vec2 h) {
    vec2 q = abs(p - c) - h;
    return 1.0 - smoothstep(0.0, 0.012, max(q.x, q.y));
}
void main() {
    float aspect = RENDERSIZE.x / RENDERSIZE.y;
    vec2 p = (isf_FragNormCoord - 0.5) * vec2(aspect, 1.0) / size;
    float w = 0.5;
    float h = 0.9;
    float cellw = 0.8;
    float idx = floor(p.x / cellw + 2.0);
    vec3 col = background.rgb;
    if (idx >= 0.0 && idx < 4.0) {
        int value = int(mod(floor(TIME * rate), 10000.0));
        int div = idx < 0.5 ? 1000 : (idx < 1.5 ? 100 : (idx < 2.5 ? 10 : 1));
        int bits = glyph((value / div) % 10);
        vec2 q = vec2(p.x - (idx - 1.5) * cellw, p.y) * vec2(1.0, 1.0);
        float t = thickness * 0.5;
        float lw = w * 0.5 - t * 1.2;
        float lh = h * 0.25 - t * 0.6;
        float on = 0.0;
        float off = 0.0;
        for (int s = 0; s < 7; s++) {
            vec2 c;
            vec2 hs;
            if (s == 0) { c = vec2(0.0, h * 0.5); hs = vec2(lw, t); }
            else if (s == 1) { c = vec2(w * 0.5 - t, h * 0.25); hs = vec2(t, lh); }
            else if (s == 2) { c = vec2(w * 0.5 - t, -h * 0.25); hs = vec2(t, lh); }
            else if (s == 3) { c = vec2(0.0, -h * 0.5); hs = vec2(lw, t); }
            else if (s == 4) { c = vec2(-w * 0.5 + t, -h * 0.25); hs = vec2(t, lh); }
            else if (s == 5) { c = vec2(-w * 0.5 + t, h * 0.25); hs = vec2(t, lh); }
            else { c = vec2(0.0, 0.0); hs = vec2(lw, t); }
            float b = bar(q, c, hs);
            if (((bits >> s) & 1) == 1) { on = max(on, b); } else { off = max(off, b); }
        }
        col = mix(col, unlit.rgb, off);
        col = mix(col, lit.rgb, on);
    }
    gl_FragColor = vec4(col, 1.0);
}
