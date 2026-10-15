/*{
  "DESCRIPTION": "Rotates, zooms and offsets the image, with optional continuous spin.",
  "CATEGORIES": ["Geometry"],
  "INPUTS": [
    { "NAME": "inputImage", "TYPE": "image" },
    { "NAME": "angle", "TYPE": "float", "MIN": -180.0, "MAX": 180.0, "DEFAULT": 0.0 },
    { "NAME": "zoom", "TYPE": "float", "MIN": 0.1, "MAX": 8.0, "DEFAULT": 1.0 },
    { "NAME": "spin", "TYPE": "float", "MIN": -180.0, "MAX": 180.0, "DEFAULT": 0.0 },
    { "NAME": "offset", "TYPE": "point2D", "DEFAULT": [0.0, 0.0] }
  ]
}*/
void main() {
    float aspect = RENDERSIZE.x / RENDERSIZE.y;
    vec2 p = (isf_FragNormCoord - 0.5 - offset) * vec2(aspect, 1.0) / zoom;
    float a = radians(angle + spin * TIME);
    float s = sin(a);
    float c = cos(a);
    p = vec2(c * p.x - s * p.y, s * p.x + c * p.y);
    gl_FragColor = IMG_NORM_PIXEL(inputImage, p / vec2(aspect, 1.0) + 0.5);
}
