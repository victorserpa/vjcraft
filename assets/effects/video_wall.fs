/*{
  "DESCRIPTION": "VideoWall: splits the picture across N x M tiles, each tile showing its own part, with a gap between them.",
  "CATEGORIES": ["Geometry"],
  "INPUTS": [
    { "NAME": "inputImage", "TYPE": "image" },
    { "NAME": "columns", "TYPE": "float", "MIN": 1.0, "MAX": 12.0, "DEFAULT": 3.0 },
    { "NAME": "rows", "TYPE": "float", "MIN": 1.0, "MAX": 12.0, "DEFAULT": 2.0 },
    { "NAME": "gap", "TYPE": "float", "MIN": 0.0, "MAX": 0.5, "DEFAULT": 0.06 },
    { "NAME": "gap_color", "TYPE": "color", "DEFAULT": [0.0, 0.0, 0.0, 1.0] }
  ]
}*/
void main() {
    vec2 uv = isf_FragNormCoord;
    vec2 n = vec2(floor(columns), floor(rows));
    vec2 g = uv * n;
    vec2 id = min(floor(g), n - 1.0);
    vec2 f = g - id;
    // Gap is a fraction of the tile; the picture part keeps the tile's slice of the image.
    vec2 inner = (f - 0.5) / max(1.0 - gap, 0.05) + 0.5;
    vec4 c = vec4(gap_color.rgb, 1.0);
    if (inner.x >= 0.0 && inner.x <= 1.0 && inner.y >= 0.0 && inner.y <= 1.0) {
        vec4 t = IMG_NORM_PIXEL(inputImage, (id + inner) / n);
        c = vec4(mix(c.rgb, t.rgb, t.a), 1.0);
    }
    gl_FragColor = c;
}
