/*{
  "DESCRIPTION": "Layered scrolling sine ribbons, back to front.",
  "CATEGORIES": ["Generator"],
  "INPUTS": [
    { "NAME": "background", "TYPE": "color", "DEFAULT": [0.02, 0.03, 0.12, 1.0] },
    { "NAME": "ribbon", "TYPE": "color", "DEFAULT": [0.2, 0.7, 1.0, 1.0] },
    { "NAME": "ribbon_far", "TYPE": "color", "DEFAULT": [0.9, 0.3, 0.7, 1.0] },
    { "NAME": "layers", "TYPE": "float", "MIN": 1.0, "MAX": 8.0, "DEFAULT": 5.0 },
    { "NAME": "frequency", "TYPE": "float", "MIN": 0.5, "MAX": 12.0, "DEFAULT": 3.0 },
    { "NAME": "amplitude", "TYPE": "float", "MIN": 0.0, "MAX": 0.4, "DEFAULT": 0.12 },
    { "NAME": "speed", "TYPE": "float", "MIN": 0.0, "MAX": 4.0, "DEFAULT": 0.7 }
  ]
}*/
void main() {
    vec2 uv = isf_FragNormCoord;
    vec3 col = background.rgb;
    float n = floor(layers);
    for (int i = 0; i < 8; i++) {
        if (float(i) >= n) { break; }
        float k = float(i) / max(n - 1.0, 1.0);
        float y = 0.2 + 0.6 * k + amplitude * sin(uv.x * frequency * (1.0 + 0.3 * k) + TIME * speed * (1.0 + k) + float(i) * 1.7);
        float m = smoothstep(y + 0.004, y - 0.004, uv.y);
        vec3 tone = mix(ribbon.rgb, ribbon_far.rgb, k) * (0.35 + 0.65 * (1.0 - k));
        col = mix(col, tone, m);
    }
    gl_FragColor = vec4(col, 1.0);
}
