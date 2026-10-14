/*{
  "DESCRIPTION": "Lens fringing: colour channels scale apart toward the edges.",
  "CATEGORIES": ["Stylize"],
  "INPUTS": [
    { "NAME": "inputImage", "TYPE": "image" },
    { "NAME": "amount", "TYPE": "float", "MIN": 0.0, "MAX": 0.1, "DEFAULT": 0.012 },
    { "NAME": "center", "TYPE": "point2D", "MIN": [0.0, 0.0], "MAX": [1.0, 1.0], "DEFAULT": [0.5, 0.5] }
  ]
}*/
void main() {
    vec2 p = isf_FragNormCoord - center;
    vec4 g = IMG_THIS_PIXEL(inputImage);
    float r = IMG_NORM_PIXEL(inputImage, center + p * (1.0 + amount)).r;
    float b = IMG_NORM_PIXEL(inputImage, center + p * (1.0 - amount)).b;
    gl_FragColor = vec4(r, g.g, b, g.a);
}
