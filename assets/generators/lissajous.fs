/*{
  "DESCRIPTION": "Lissajous curve drawn as a glowing line; frequencies and phase drift over time.",
  "CATEGORIES": ["Generator"],
  "INPUTS": [
    { "NAME": "lineColor", "TYPE": "color", "DEFAULT": [0.4, 1.0, 0.9, 1.0] },
    { "NAME": "background", "TYPE": "color", "DEFAULT": [0.02, 0.02, 0.06, 1.0] },
    { "NAME": "freqX", "TYPE": "float", "MIN": 1.0, "MAX": 9.0, "DEFAULT": 3.0 },
    { "NAME": "freqY", "TYPE": "float", "MIN": 1.0, "MAX": 9.0, "DEFAULT": 2.0 },
    { "NAME": "drift", "TYPE": "float", "MIN": 0.0, "MAX": 2.0, "DEFAULT": 0.3 },
    { "NAME": "size", "TYPE": "float", "MIN": 0.1, "MAX": 0.5, "DEFAULT": 0.38 },
    { "NAME": "thickness", "TYPE": "float", "MIN": 0.002, "MAX": 0.04, "DEFAULT": 0.006 }
  ]
}*/
vec2 curve_(float t, float ph) {
    return vec2(sin(freqX * t + ph), sin(freqY * t));
}
float seg_(vec2 p, vec2 a, vec2 b) {
    vec2 pa = p - a;
    vec2 ba = b - a;
    float h = clamp(dot(pa, ba) / max(dot(ba, ba), 1e-9), 0.0, 1.0);
    return length(pa - ba * h);
}
void main() {
    float aspect = RENDERSIZE.x / RENDERSIZE.y;
    vec2 p = (isf_FragNormCoord - 0.5) * vec2(aspect, 1.0);
    float ph = TIME * drift;
    float d = 10.0;
    vec2 prev = curve_(0.0, ph) * size;
    for (int i = 1; i <= 160; i++) {
        float t = 6.2831853 * float(i) / 160.0;
        vec2 cur = curve_(t, ph) * size;
        d = min(d, seg_(p, prev, cur));
        prev = cur;
    }
    float core = 1.0 - smoothstep(thickness, thickness * 1.8, d);
    float halo = thickness * 4.0 / (d + thickness * 4.0);
    vec3 col = background.rgb + lineColor.rgb * (core + halo * halo * 0.5);
    gl_FragColor = vec4(clamp(col, 0.0, 1.0), 1.0);
}
