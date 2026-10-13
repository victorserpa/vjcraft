/*{
  "DESCRIPTION": "Offsets the red and blue channels in opposite directions.",
  "CATEGORIES": ["Distort"],
  "INPUTS": [
    { "NAME": "inputImage", "TYPE": "image" },
    { "NAME": "distance", "TYPE": "float", "MIN": 0.0, "MAX": 0.1, "DEFAULT": 0.01 },
    { "NAME": "angle", "TYPE": "float", "MIN": 0.0, "MAX": 6.2831853, "DEFAULT": 0.0 }
  ]
}*/
void main() {
    vec2 d = vec2(cos(angle), sin(angle)) * distance;
    vec4 g = IMG_NORM_PIXEL(inputImage, isf_FragNormCoord);
    float r = IMG_NORM_PIXEL(inputImage, isf_FragNormCoord + d).r;
    float b = IMG_NORM_PIXEL(inputImage, isf_FragNormCoord - d).b;
    gl_FragColor = vec4(r, g.g, b, g.a);
}
