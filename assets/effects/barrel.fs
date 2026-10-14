/*{
  "DESCRIPTION": "Lens distortion: bows straight lines outward or inward.",
  "CATEGORIES": ["Distort"],
  "INPUTS": [
    { "NAME": "inputImage", "TYPE": "image" },
    { "NAME": "curve", "TYPE": "float", "MIN": -1.0, "MAX": 1.0, "DEFAULT": 0.3 },
    { "NAME": "zoom", "TYPE": "float", "MIN": 0.5, "MAX": 2.0, "DEFAULT": 1.0 }
  ]
}*/
void main() {
    vec2 p = (isf_FragNormCoord - vec2(0.5)) * 2.0 / zoom;
    float r2 = dot(p, p);
    p *= 1.0 + curve * r2;
    gl_FragColor = IMG_NORM_PIXEL(inputImage, p * 0.5 + vec2(0.5));
}
