/*{
  "DESCRIPTION": "Fisheye lens: magnifies the centre and squeezes the rim.",
  "CATEGORIES": ["Distort"],
  "INPUTS": [
    { "NAME": "inputImage", "TYPE": "image" },
    { "NAME": "strength", "TYPE": "float", "MIN": 0.0, "MAX": 1.0, "DEFAULT": 0.6 }
  ]
}*/
void main() {
    vec2 p = isf_FragNormCoord - 0.5;
    float r = length(p) * 2.0;
    // Sample closer to the centre in the middle (magnify), full range at the rim.
    gl_FragColor = IMG_NORM_PIXEL(inputImage, 0.5 + p * mix(1.0, r, strength));
}
