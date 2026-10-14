/*{
  "DESCRIPTION": "Magnifies (positive) or pinches (negative) around a centre point.",
  "CATEGORIES": ["Distort"],
  "INPUTS": [
    { "NAME": "inputImage", "TYPE": "image" },
    { "NAME": "strength", "TYPE": "float", "MIN": -1.0, "MAX": 1.0, "DEFAULT": 0.5 },
    { "NAME": "radius", "TYPE": "float", "MIN": 0.05, "MAX": 1.5, "DEFAULT": 0.5 },
    { "NAME": "center", "TYPE": "point2D", "MIN": [0.0, 0.0], "MAX": [1.0, 1.0], "DEFAULT": [0.5, 0.5] }
  ]
}*/
void main() {
    float aspect = RENDERSIZE.x / RENDERSIZE.y;
    vec2 p = (isf_FragNormCoord - center) * vec2(aspect, 1.0);
    float d = length(p) / radius;
    float k = d < 1.0 ? pow(d, 1.0 + strength) / max(d, 0.0001) : 1.0;
    p *= k;
    gl_FragColor = IMG_NORM_PIXEL(inputImage, center + p / vec2(aspect, 1.0));
}
