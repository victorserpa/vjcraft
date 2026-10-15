/*{
  "DESCRIPTION": "Mosaic in polar cells: rings and wedges around a centre point.",
  "CATEGORIES": ["Stylize"],
  "INPUTS": [
    { "NAME": "inputImage", "TYPE": "image" },
    { "NAME": "center", "TYPE": "point2D", "MIN": [0.0, 0.0], "MAX": [1.0, 1.0], "DEFAULT": [0.5, 0.5] },
    { "NAME": "rings", "TYPE": "float", "MIN": 2.0, "MAX": 60.0, "DEFAULT": 12.0 },
    { "NAME": "wedges", "TYPE": "float", "MIN": 3.0, "MAX": 96.0, "DEFAULT": 24.0 },
    { "NAME": "spin", "TYPE": "float", "MIN": -2.0, "MAX": 2.0, "DEFAULT": 0.0 }
  ]
}*/
void main() {
    float aspect = RENDERSIZE.x / RENDERSIZE.y;
    vec2 p = (isf_FragNormCoord - center) * vec2(aspect, 1.0);
    float r = length(p);
    float a = atan(p.y, p.x) + TIME * spin;
    float rr = (floor(r * rings) + 0.5) / rings;
    float step_a = 6.2831853 / floor(wedges);
    float aa = (floor(a / step_a) + 0.5) * step_a - TIME * spin;
    vec2 q = vec2(cos(aa), sin(aa)) * rr;
    q.x /= aspect;
    gl_FragColor = IMG_NORM_PIXEL(inputImage, q + center);
}
