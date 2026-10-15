/*{
  "DESCRIPTION": "Pen cross-hatching: darker areas get more layers of crossing lines.",
  "CATEGORIES": ["Stylize"],
  "INPUTS": [
    { "NAME": "inputImage", "TYPE": "image" },
    { "NAME": "ink", "TYPE": "color", "DEFAULT": [0.05, 0.04, 0.1, 1.0] },
    { "NAME": "density", "TYPE": "float", "MIN": 10.0, "MAX": 200.0, "DEFAULT": 70.0 },
    { "NAME": "line_width", "TYPE": "float", "MIN": 0.05, "MAX": 0.6, "DEFAULT": 0.25 },
    { "NAME": "keep_color", "TYPE": "float", "MIN": 0.0, "MAX": 1.0, "DEFAULT": 0.3 }
  ]
}*/
float line_(vec2 p, float ang, float dens) {
    float u = dot(p, vec2(cos(ang), sin(ang))) * dens;
    float f = abs(fract(u) - 0.5) * 2.0;
    float e = fwidth(u) * 2.0 + 1e-4;
    return 1.0 - smoothstep(line_width * 2.0 - e, line_width * 2.0 + e, 1.0 - f);
}
void main() {
    float aspect = RENDERSIZE.x / RENDERSIZE.y;
    vec2 p = isf_FragNormCoord * vec2(aspect, 1.0);
    vec4 c = IMG_THIS_PIXEL(inputImage);
    float l = dot(c.rgb, vec3(0.299, 0.587, 0.114));
    float h = 0.0;
    h = max(h, line_(p, 0.785, density) * step(l, 0.8));
    h = max(h, line_(p, 2.356, density) * step(l, 0.6));
    h = max(h, line_(p, 0.0, density * 1.1) * step(l, 0.4));
    h = max(h, line_(p, 1.571, density * 1.1) * step(l, 0.2));
    vec3 paper = mix(vec3(1.0), c.rgb, keep_color);
    gl_FragColor = vec4(mix(paper, ink.rgb, h), c.a);
}
