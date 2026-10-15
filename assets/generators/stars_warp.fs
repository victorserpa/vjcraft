/*{
  "DESCRIPTION": "Hyperspace: stars streak outward from the centre like a warp jump.",
  "CATEGORIES": ["Generator"],
  "INPUTS": [
    { "NAME": "starColor", "TYPE": "color", "DEFAULT": [0.8, 0.9, 1.0, 1.0] },
    { "NAME": "background", "TYPE": "color", "DEFAULT": [0.0, 0.0, 0.04, 1.0] },
    { "NAME": "speed", "TYPE": "float", "MIN": 0.0, "MAX": 3.0, "DEFAULT": 0.6 },
    { "NAME": "streak", "TYPE": "float", "MIN": 0.0, "MAX": 1.0, "DEFAULT": 0.5 },
    { "NAME": "density", "TYPE": "float", "MIN": 20.0, "MAX": 300.0, "DEFAULT": 120.0 },
    { "NAME": "center", "TYPE": "point2D", "MIN": [0.0, 0.0], "MAX": [1.0, 1.0], "DEFAULT": [0.5, 0.5] }
  ]
}*/
float h1_(float n) {
    return fract(sin(n * 91.3458) * 47453.5453);
}
void main() {
    float aspect = RENDERSIZE.x / RENDERSIZE.y;
    vec2 p = (isf_FragNormCoord - center) * vec2(aspect, 1.0);
    float r = length(p);
    float a = atan(p.y, p.x) / 6.2831853 + 0.5;
    vec3 col = background.rgb;
    for (int l = 0; l < 3; l++) {
        float fl = float(l);
        float n = density * (1.0 + fl * 0.6);
        float cell = floor(a * n);
        float lat = (fract(a * n) - 0.5) * r * 6.2831853 / n;
        float seed = cell + fl * 137.0;
        float z = fract(h1_(seed) + TIME * speed * (0.4 + 0.3 * fl));
        float head = z * z * 1.2;
        float tail = head * (0.15 + streak * 0.6);
        float along = smoothstep(head - tail, head, r) * (1.0 - smoothstep(head, head + 0.01, r));
        float width = 0.0012 + 0.0025 * z;
        float side = 1.0 - smoothstep(width * 0.3, width, abs(lat));
        float vis = step(0.35, h1_(seed + 7.0));
        col += starColor.rgb * along * side * vis * (0.4 + z);
    }
    gl_FragColor = vec4(clamp(col, 0.0, 1.0), 1.0);
}
