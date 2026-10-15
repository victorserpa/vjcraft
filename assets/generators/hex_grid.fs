/*{
  "DESCRIPTION": "Hexagonal grid with glowing edges and cells that pulse at random.",
  "CATEGORIES": ["Generator"],
  "INPUTS": [
    { "NAME": "edgeColor", "TYPE": "color", "DEFAULT": [0.2, 0.8, 1.0, 1.0] },
    { "NAME": "fillColor", "TYPE": "color", "DEFAULT": [0.9, 0.3, 0.6, 1.0] },
    { "NAME": "background", "TYPE": "color", "DEFAULT": [0.02, 0.03, 0.08, 1.0] },
    { "NAME": "scale", "TYPE": "float", "MIN": 2.0, "MAX": 40.0, "DEFAULT": 8.0 },
    { "NAME": "edgeWidth", "TYPE": "float", "MIN": 0.01, "MAX": 0.3, "DEFAULT": 0.06 },
    { "NAME": "pulse", "TYPE": "float", "MIN": 0.0, "MAX": 4.0, "DEFAULT": 1.0 }
  ]
}*/
float h2_(vec2 p) {
    return fract(sin(dot(p, vec2(127.1, 311.7))) * 43758.5453);
}
// Returns (distance to nearest hex edge, cell id).
vec4 hex_(vec2 p) {
    vec2 r = vec2(1.0, 1.7320508);
    vec2 h = r * 0.5;
    vec2 a = mod(p, r) - h;
    vec2 b = mod(p - h, r) - h;
    vec2 g = dot(a, a) < dot(b, b) ? a : b;
    vec2 id = p - g;
    vec2 q = abs(g);
    float d = max(dot(q, normalize(vec2(1.0, 1.7320508))), q.x);
    return vec4(0.5 - d, id, 0.0);
}
void main() {
    float aspect = RENDERSIZE.x / RENDERSIZE.y;
    vec2 p = (isf_FragNormCoord - 0.5) * vec2(aspect, 1.0) * scale;
    vec4 h = hex_(p);
    float rnd = h2_(h.yz);
    float on = 0.5 + 0.5 * sin(TIME * pulse * (0.5 + rnd * 2.0) + rnd * 20.0);
    on = pow(on, 3.0);
    float e = 1.0 - smoothstep(0.0, edgeWidth, h.x);
    vec3 col = mix(background.rgb, fillColor.rgb, on * 0.8 * step(0.35, rnd));
    col = mix(col, edgeColor.rgb, e);
    col += edgeColor.rgb * 0.15 * (1.0 - smoothstep(0.0, edgeWidth * 4.0, h.x));
    gl_FragColor = vec4(col, 1.0);
}
