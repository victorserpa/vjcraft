/*{
  "DESCRIPTION": "Animated cellular pattern with coloured cells and bright borders.",
  "CATEGORIES": ["Generator"],
  "INPUTS": [
    { "NAME": "colorA", "TYPE": "color", "DEFAULT": [0.05, 0.1, 0.3, 1.0] },
    { "NAME": "colorB", "TYPE": "color", "DEFAULT": [0.2, 0.9, 0.7, 1.0] },
    { "NAME": "border", "TYPE": "color", "DEFAULT": [1.0, 1.0, 1.0, 1.0] },
    { "NAME": "cells", "TYPE": "float", "MIN": 2.0, "MAX": 40.0, "DEFAULT": 8.0 },
    { "NAME": "speed", "TYPE": "float", "MIN": 0.0, "MAX": 4.0, "DEFAULT": 0.5 },
    { "NAME": "line_width", "TYPE": "float", "MIN": 0.0, "MAX": 0.3, "DEFAULT": 0.05 }
  ]
}*/
vec2 hash2(vec2 p) {
    return fract(sin(vec2(dot(p, vec2(127.1, 311.7)), dot(p, vec2(269.5, 183.3)))) * 43758.5453);
}
void main() {
    float aspect = RENDERSIZE.x / RENDERSIZE.y;
    vec2 g = isf_FragNormCoord * vec2(aspect, 1.0) * cells;
    vec2 base = floor(g);
    float d1 = 9.0;
    float d2 = 9.0;
    vec2 best = vec2(0.0);
    for (int j = -1; j <= 1; j++) {
        for (int i = -1; i <= 1; i++) {
            vec2 n = base + vec2(float(i), float(j));
            vec2 h = hash2(n);
            vec2 pt = n + 0.5 + 0.45 * sin(6.2831853 * h + TIME * speed);
            float d = length(g - pt);
            if (d < d1) {
                d2 = d1;
                d1 = d;
                best = n;
            } else if (d < d2) {
                d2 = d;
            }
        }
    }
    float shade = hash2(best).x;
    vec3 c = mix(colorA.rgb, colorB.rgb, shade);
    float edge = 1.0 - smoothstep(line_width * 0.5, line_width * 0.5 + 0.03, d2 - d1);
    gl_FragColor = vec4(mix(c, border.rgb, edge * step(0.0001, line_width)), 1.0);
}
