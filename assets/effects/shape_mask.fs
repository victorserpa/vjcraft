/*{
  "DESCRIPTION": "Shows the layer only inside a rectangle, ellipse or regular polygon, with a soft edge.",
  "CATEGORIES": ["Mask"],
  "INPUTS": [
    { "NAME": "inputImage", "TYPE": "image" },
    { "NAME": "shape", "TYPE": "long", "VALUES": [0, 1, 2], "LABELS": ["Rectangle", "Ellipse", "Polygon"], "DEFAULT": 0 },
    { "NAME": "center", "TYPE": "point2D", "MIN": [0.0, 0.0], "MAX": [1.0, 1.0], "DEFAULT": [0.5, 0.5] },
    { "NAME": "width", "TYPE": "float", "MIN": 0.0, "MAX": 1.0, "DEFAULT": 0.6 },
    { "NAME": "height", "TYPE": "float", "MIN": 0.0, "MAX": 1.0, "DEFAULT": 0.6 },
    { "NAME": "sides", "TYPE": "long", "VALUES": [3, 4, 5, 6, 8], "LABELS": ["3", "4", "5", "6", "8"], "DEFAULT": 3 },
    { "NAME": "rotation", "TYPE": "float", "MIN": 0.0, "MAX": 360.0, "DEFAULT": 0.0 },
    { "NAME": "feather", "TYPE": "float", "MIN": 0.0, "MAX": 1.0, "DEFAULT": 0.05 },
    { "NAME": "invert", "TYPE": "bool", "DEFAULT": false }
  ]
}*/
void main() {
    vec4 c = IMG_THIS_PIXEL(inputImage);
    vec2 half_size = max(vec2(width, height) * 0.5, vec2(0.0001));
    vec2 p = isf_FragNormCoord - center;
    float a = radians(rotation);
    p = vec2(cos(a) * p.x + sin(a) * p.y, -sin(a) * p.x + cos(a) * p.y) / half_size;
    // d < 0 inside, in units of the half size.
    float d;
    if (shape == 0) {
        d = max(abs(p.x), abs(p.y)) - 1.0;
    } else if (shape == 1) {
        d = length(p) - 1.0;
    } else {
        // `sides` is the VALUES entry itself (3, 4, 5, 6 or 8), not its index.
        float n = float(max(sides, 3));
        float seg = 6.2831853 / n;
        float t = mod(atan(p.y, p.x) + seg * 0.5, seg) - seg * 0.5;
        d = length(p) * cos(t) - cos(seg * 0.5);
    }
    float f = max(feather, 0.0005);
    float m = 1.0 - smoothstep(-f, f, d);
    if (invert) { m = 1.0 - m; }
    gl_FragColor = vec4(c.rgb, c.a * m);
}
