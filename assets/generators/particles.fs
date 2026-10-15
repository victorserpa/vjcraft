/*{
  "DESCRIPTION": "Soft glowing particles drifting upward (hash-based, no state).",
  "CATEGORIES": ["Generator"],
  "INPUTS": [
    { "NAME": "background", "TYPE": "color", "DEFAULT": [0.01, 0.01, 0.06, 1.0] },
    { "NAME": "colorA", "TYPE": "color", "DEFAULT": [0.4, 0.8, 1.0, 1.0] },
    { "NAME": "colorB", "TYPE": "color", "DEFAULT": [1.0, 0.5, 0.8, 1.0] },
    { "NAME": "density", "TYPE": "float", "MIN": 2.0, "MAX": 30.0, "DEFAULT": 10.0 },
    { "NAME": "size", "TYPE": "float", "MIN": 0.01, "MAX": 0.4, "DEFAULT": 0.12 },
    { "NAME": "speed", "TYPE": "float", "MIN": -2.0, "MAX": 2.0, "DEFAULT": 0.3 }
  ]
}*/
vec2 hash2(vec2 p) {
    return fract(sin(vec2(dot(p, vec2(127.1, 311.7)), dot(p, vec2(269.5, 183.3)))) * 43758.5453);
}
void main() {
    float aspect = RENDERSIZE.x / RENDERSIZE.y;
    vec3 acc = vec3(0.0);
    for (int layer = 0; layer < 3; layer++) {
        float fl = float(layer);
        float sc = density * (1.0 + fl * 0.6);
        vec2 g = isf_FragNormCoord * vec2(aspect, 1.0) * sc;
        g.y -= TIME * speed * sc * (0.4 + 0.3 * fl);
        vec2 id = floor(g);
        for (int j = -1; j <= 1; j++) {
            for (int i = -1; i <= 1; i++) {
                vec2 n = id + vec2(float(i), float(j)) + fl * 31.0;
                vec2 h = hash2(n);
                vec2 pos = vec2(float(i), float(j)) + h - fract(g) + 0.0;
                pos.x += 0.3 * sin(TIME * (0.5 + h.y) + h.x * 6.2831853);
                float d = length(pos);
                float s = size * (0.5 + h.y) * (1.0 - 0.25 * fl);
                float glow = s * s / (d * d + s * s * 0.5) * 0.4;
                glow *= 1.0 - smoothstep(0.7, 1.6, d);
                acc += mix(colorA.rgb, colorB.rgb, h.x) * glow * (0.5 + 0.5 * h.y);
            }
        }
    }
    gl_FragColor = vec4(background.rgb + acc, 1.0);
}
