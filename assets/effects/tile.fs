/*{
  "DESCRIPTION": "Repeats the image in a grid.",
  "CATEGORIES": ["Geometry"],
  "INPUTS": [
    { "NAME": "inputImage", "TYPE": "image" },
    { "NAME": "count", "TYPE": "float", "MIN": 1.0, "MAX": 16.0, "DEFAULT": 2.0 },
    { "NAME": "offset", "TYPE": "point2D", "MIN": [0.0, 0.0], "MAX": [1.0, 1.0], "DEFAULT": [0.0, 0.0] }
  ]
}*/
void main() {
    vec2 uv = fract(isf_FragNormCoord * floor(count) + offset);
    gl_FragColor = IMG_NORM_PIXEL(inputImage, uv);
}
