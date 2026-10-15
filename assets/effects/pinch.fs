/*{
  "DESCRIPTION": "Pulls pixels toward a centre point.",
  "CATEGORIES": ["Distort"],
  "INPUTS": [
    { "NAME": "inputImage", "TYPE": "image" },
    { "NAME": "amount", "TYPE": "float", "MIN": -1.0, "MAX": 1.0, "DEFAULT": 0.5 },
    { "NAME": "radius", "TYPE": "float", "MIN": 0.05, "MAX": 1.0, "DEFAULT": 0.5 },
    { "NAME": "center", "TYPE": "point2D", "DEFAULT": [0.5, 0.5] }
  ]
}*/
void main() {
    float aspect = RENDERSIZE.x / RENDERSIZE.y;
    vec2 off = isf_FragNormCoord - center;
    float r = length(off * vec2(aspect, 1.0)) / radius;
    // Inside the radius, sample further out near the centre: content shrinks into the centre.
    float f = r < 1.0 ? 1.0 + amount * (1.0 - r) * (1.0 - r) : 1.0;
    gl_FragColor = IMG_NORM_PIXEL(inputImage, center + off * f);
}
