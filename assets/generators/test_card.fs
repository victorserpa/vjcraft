/*{
  "DESCRIPTION": "Test card: colour bars, grey ramp, alignment grid, circle and centre cross for checking an output.",
  "CATEGORIES": ["Generator"],
  "INPUTS": [
    { "NAME": "grid", "TYPE": "bool", "DEFAULT": true },
    { "NAME": "circle", "TYPE": "bool", "DEFAULT": true },
    { "NAME": "grid_cells", "TYPE": "float", "MIN": 4.0, "MAX": 32.0, "DEFAULT": 16.0 },
    { "NAME": "brightness", "TYPE": "float", "MIN": 0.1, "MAX": 1.0, "DEFAULT": 0.75 }
  ]
}*/
void main() {
    vec2 uv = isf_FragNormCoord;
    float aspect = RENDERSIZE.x / RENDERSIZE.y;
    vec3 col;
    if (uv.y > 0.34) {
        float b = floor(uv.x * 7.0);
        col = vec3(mod(b, 2.0) >= 1.0 ? 1.0 : 0.0, mod(floor(b / 2.0), 2.0) >= 1.0 ? 1.0 : 0.0, b >= 4.0 ? 1.0 : 0.0);
        col = b < 0.5 ? vec3(1.0) : col;
        col *= brightness;
    } else if (uv.y > 0.2) {
        col = vec3(floor(uv.x * 11.0) / 10.0);
    } else {
        col = vec3(0.08 + 0.1 * step(0.5, fract(uv.x * 8.0)));
    }
    if (grid) {
        vec2 g = abs(fract(uv * vec2(grid_cells * aspect / 1.7778, grid_cells * 0.5625)) - 0.5);
        vec2 w = 0.5 - 1.5 / RENDERSIZE * vec2(grid_cells * aspect / 1.7778, grid_cells * 0.5625);
        if (max(g.x, g.y) > max(w.x, w.y)) { col = mix(col, vec3(1.0), 0.5); }
    }
    vec2 p = (uv - 0.5) * vec2(aspect, 1.0);
    if (circle && abs(length(p) - 0.45) < 1.5 / RENDERSIZE.y) { col = vec3(1.0, 1.0, 0.0); }
    if (min(abs(p.x), abs(p.y)) < 0.75 / RENDERSIZE.y && max(abs(p.x), abs(p.y)) < 0.04) { col = vec3(1.0, 0.2, 0.2); }
    gl_FragColor = vec4(col, 1.0);
}
