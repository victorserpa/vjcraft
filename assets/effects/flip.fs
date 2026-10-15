/*{
  "DESCRIPTION": "Flips the image horizontally and/or vertically.",
  "CATEGORIES": ["Geometry"],
  "INPUTS": [
    { "NAME": "inputImage", "TYPE": "image" },
    { "NAME": "horizontal", "TYPE": "bool", "DEFAULT": true },
    { "NAME": "vertical", "TYPE": "bool", "DEFAULT": false }
  ]
}*/
void main() {
    vec2 uv = isf_FragNormCoord;
    if (horizontal) {
        uv.x = 1.0 - uv.x;
    }
    if (vertical) {
        uv.y = 1.0 - uv.y;
    }
    gl_FragColor = IMG_NORM_PIXEL(inputImage, uv);
}
