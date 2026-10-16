/*{
  "DESCRIPTION": "Shaper: a polygon or ring with soft edges that can spin and pulse.",
  "CATEGORIES": ["Generator"],
  "INPUTS": [
    { "NAME": "fill", "TYPE": "color", "DEFAULT": [1.0, 0.8, 0.2, 1.0] },
    { "NAME": "background", "TYPE": "color", "DEFAULT": [0.05, 0.05, 0.12, 1.0] },
    { "NAME": "sides", "TYPE": "float", "MIN": 3.0, "MAX": 12.0, "DEFAULT": 5.0 },
    { "NAME": "size", "TYPE": "float", "MIN": 0.05, "MAX": 0.7, "DEFAULT": 0.3 },
    { "NAME": "outline", "TYPE": "bool", "DEFAULT": false },
    { "NAME": "line_width", "TYPE": "float", "MIN": 0.005, "MAX": 0.2, "DEFAULT": 0.04 },
    { "NAME": "softness", "TYPE": "float", "MIN": 0.0, "MAX": 0.2, "DEFAULT": 0.01 },
    { "NAME": "spin", "TYPE": "float", "MIN": -180.0, "MAX": 180.0, "DEFAULT": 20.0 },
    { "NAME": "pulse", "TYPE": "float", "MIN": 0.0, "MAX": 0.5, "DEFAULT": 0.0 }
  ]
}*/
void main() {
    float aspect = RENDERSIZE.x / RENDERSIZE.y;
    vec2 p = (isf_FragNormCoord - 0.5) * vec2(aspect, 1.0);
    float n = floor(sides);
    float a = atan(p.y, p.x) + radians(spin) * TIME;
    float k = 6.2831853 / n;
    float r = size * (1.0 + pulse * sin(TIME * 3.0));
    float d = cos(floor(0.5 + a / k) * k - a) * length(p) - r;
    if (outline) { d = abs(d) - line_width * 0.5; }
    float m = 1.0 - smoothstep(-softness - 0.0005, softness + 0.0005, d);
    gl_FragColor = vec4(mix(background.rgb, fill.rgb, m), 1.0);
}
