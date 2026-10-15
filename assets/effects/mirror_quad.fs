/*{
  "DESCRIPTION": "Four-way mirror around a movable centre.",
  "CATEGORIES": ["Geometry"],
  "INPUTS": [
    { "NAME": "inputImage", "TYPE": "image" },
    { "NAME": "center", "TYPE": "point2D", "DEFAULT": [0.5, 0.5] }
  ]
}*/
void main() {
    vec2 uv = isf_FragNormCoord;
    // Fold both axes onto the quadrant that contains the origin side of the centre.
    vec2 d = abs(uv - center);
    gl_FragColor = IMG_NORM_PIXEL(inputImage, center - d);
}
