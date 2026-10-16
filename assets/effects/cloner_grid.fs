/*{
  "DESCRIPTION": "Cloner Grid: copies of the whole picture in a grid, with a gap, an offset on alternate rows and a shrink per column.",
  "CATEGORIES": ["Geometry"],
  "INPUTS": [
    { "NAME": "inputImage", "TYPE": "image" },
    { "NAME": "columns", "TYPE": "float", "MIN": 1.0, "MAX": 12.0, "DEFAULT": 3.0 },
    { "NAME": "rows", "TYPE": "float", "MIN": 1.0, "MAX": 12.0, "DEFAULT": 3.0 },
    { "NAME": "gap", "TYPE": "float", "MIN": 0.0, "MAX": 0.6, "DEFAULT": 0.1 },
    { "NAME": "row_offset", "TYPE": "float", "MIN": 0.0, "MAX": 1.0, "DEFAULT": 0.0 },
    { "NAME": "shrink", "TYPE": "float", "MIN": 0.0, "MAX": 0.9, "DEFAULT": 0.0 }
  ]
}*/
void main() {
    vec2 uv = isf_FragNormCoord;
    float nc = floor(columns);
    float nr = floor(rows);
    float row = floor(uv.y * nr);
    float shifted = uv.x * nc - row_offset * 0.5 * mod(row, 2.0);
    float col = floor(shifted);
    vec2 f = vec2(fract(shifted), fract(uv.y * nr));
    float s = 1.0 - shrink * (col + 1.0) / (nc + 1.0);
    f = (f - 0.5) / max(s, 0.05) + 0.5;
    f = (f - 0.5) / max(1.0 - gap, 0.05) + 0.5;
    vec4 c = vec4(0.0, 0.0, 0.0, 1.0);
    if (f.x >= 0.0 && f.x <= 1.0 && f.y >= 0.0 && f.y <= 1.0) {
        vec4 t = IMG_NORM_PIXEL(inputImage, f);
        c = vec4(mix(c.rgb, t.rgb, t.a), 1.0);
    }
    gl_FragColor = c;
}
