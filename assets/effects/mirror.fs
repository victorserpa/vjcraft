/*{
  "DESCRIPTION": "Mirrors one half of the image onto the other.",
  "CATEGORIES": ["Geometry"],
  "INPUTS": [
    { "NAME": "inputImage", "TYPE": "image" },
    { "NAME": "mode", "TYPE": "long", "VALUES": [0, 1, 2], "LABELS": ["Horizontal", "Vertical", "Both"], "DEFAULT": 0 }
  ]
}*/
void main() {
    vec2 uv = isf_FragNormCoord;
    if (mode == 0 || mode == 2) {
        uv.x = uv.x > 0.5 ? 1.0 - uv.x : uv.x;
    }
    if (mode == 1 || mode == 2) {
        uv.y = uv.y > 0.5 ? 1.0 - uv.y : uv.y;
    }
    gl_FragColor = IMG_NORM_PIXEL(inputImage, uv);
}
