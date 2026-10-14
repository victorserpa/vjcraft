/*{
  "DESCRIPTION": "Rotates pixels more the closer they are to a centre point.",
  "CATEGORIES": ["Distort"],
  "INPUTS": [
    { "NAME": "inputImage", "TYPE": "image" },
    { "NAME": "twist", "TYPE": "float", "MIN": -12.0, "MAX": 12.0, "DEFAULT": 3.0 },
    { "NAME": "radius", "TYPE": "float", "MIN": 0.05, "MAX": 1.5, "DEFAULT": 0.5 },
    { "NAME": "center", "TYPE": "point2D", "MIN": [0.0, 0.0], "MAX": [1.0, 1.0], "DEFAULT": [0.5, 0.5] }
  ]
}*/
void main() {
    float aspect = RENDERSIZE.x / RENDERSIZE.y;
    vec2 p = (isf_FragNormCoord - center) * vec2(aspect, 1.0);
    float k = 1.0 - smoothstep(0.0, radius, length(p));
    float a = twist * k * k;
    float cs = cos(a);
    float sn = sin(a);
    p = vec2(p.x * cs - p.y * sn, p.x * sn + p.y * cs);
    gl_FragColor = IMG_NORM_PIXEL(inputImage, center + p / vec2(aspect, 1.0));
}
