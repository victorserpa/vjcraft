/*{
  "DESCRIPTION": "Grid of pulsing circles.",
  "CATEGORIES": ["Generator"],
  "INPUTS": [
    { "NAME": "background", "TYPE": "color", "DEFAULT": [0.03, 0.03, 0.1, 1.0] },
    { "NAME": "colorA", "TYPE": "color", "DEFAULT": [1.0, 0.3, 0.5, 1.0] },
    { "NAME": "colorB", "TYPE": "color", "DEFAULT": [0.3, 0.8, 1.0, 1.0] },
    { "NAME": "cells", "TYPE": "float", "MIN": 2.0, "MAX": 30.0, "DEFAULT": 8.0 },
    { "NAME": "size", "TYPE": "float", "MIN": 0.1, "MAX": 0.5, "DEFAULT": 0.35 },
    { "NAME": "pulse", "TYPE": "float", "MIN": 0.0, "MAX": 1.0, "DEFAULT": 0.5 },
    { "NAME": "speed", "TYPE": "float", "MIN": 0.0, "MAX": 6.0, "DEFAULT": 2.0 }
  ]
}*/
void main() {
    float aspect = RENDERSIZE.x / RENDERSIZE.y;
    vec2 g = isf_FragNormCoord * vec2(aspect, 1.0) * cells;
    vec2 id = floor(g);
    vec2 f = fract(g) - 0.5;
    float ph = length(id - vec2(aspect, 1.0) * cells * 0.5) * 0.5;
    float r = size * (1.0 - pulse * 0.5 + pulse * 0.5 * sin(TIME * speed - ph));
    float aa = fwidth(g.x) + 0.001;
    float m = 1.0 - smoothstep(r - aa, r + aa, length(f));
    vec3 c = mix(colorA.rgb, colorB.rgb, 0.5 + 0.5 * sin(ph - TIME * speed * 0.5));
    gl_FragColor = vec4(mix(background.rgb, c, m), 1.0);
}
