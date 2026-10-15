/*{
  "DESCRIPTION": "Swirls the image, strongest at the centre and fading out at the radius.",
  "CATEGORIES": ["Distort"],
  "INPUTS": [
    { "NAME": "inputImage", "TYPE": "image" },
    { "NAME": "angle", "TYPE": "float", "MIN": -12.0, "MAX": 12.0, "DEFAULT": 4.0 },
    { "NAME": "radius", "TYPE": "float", "MIN": 0.05, "MAX": 1.5, "DEFAULT": 0.6 },
    { "NAME": "spin", "TYPE": "float", "MIN": -4.0, "MAX": 4.0, "DEFAULT": 0.0 },
    { "NAME": "center", "TYPE": "point2D", "DEFAULT": [0.5, 0.5] }
  ]
}*/
void main() {
    float aspect = RENDERSIZE.x / RENDERSIZE.y;
    vec2 d = (isf_FragNormCoord - center) * vec2(aspect, 1.0);
    float f = 1.0 - smoothstep(0.0, radius, length(d));
    float a = (angle + spin * TIME) * f * f;
    float s = sin(a);
    float c = cos(a);
    d = vec2(c * d.x - s * d.y, s * d.x + c * d.y);
    gl_FragColor = IMG_NORM_PIXEL(inputImage, center + d / vec2(aspect, 1.0));
}
