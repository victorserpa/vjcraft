/*{
  "DESCRIPTION": "Mosaic of square blocks.",
  "CATEGORIES": ["Stylize"],
  "INPUTS": [
    { "NAME": "inputImage", "TYPE": "image" },
    { "NAME": "block", "TYPE": "float", "MIN": 1.0, "MAX": 128.0, "DEFAULT": 16.0 }
  ]
}*/
void main() {
    vec2 px = isf_FragNormCoord * RENDERSIZE;
    vec2 q = (floor(px / block) + vec2(0.5)) * block / RENDERSIZE;
    gl_FragColor = IMG_NORM_PIXEL(inputImage, q);
}
