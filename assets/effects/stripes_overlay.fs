/*{
  "DESCRIPTION": "Draws coloured stripes over the picture, with angle, width and scroll speed.",
  "CATEGORIES": ["Stylize"],
  "INPUTS": [
    { "NAME": "inputImage", "TYPE": "image" },
    { "NAME": "stripe_color", "TYPE": "color", "DEFAULT": [1.0, 0.2, 0.5, 1.0] },
    { "NAME": "count", "TYPE": "float", "MIN": 1.0, "MAX": 80.0, "DEFAULT": 16.0 },
    { "NAME": "width", "TYPE": "float", "MIN": 0.02, "MAX": 0.98, "DEFAULT": 0.3 },
    { "NAME": "angle", "TYPE": "float", "MIN": 0.0, "MAX": 3.1415927, "DEFAULT": 0.6 },
    { "NAME": "speed", "TYPE": "float", "MIN": -4.0, "MAX": 4.0, "DEFAULT": 0.3 },
    { "NAME": "opacity", "TYPE": "float", "MIN": 0.0, "MAX": 1.0, "DEFAULT": 0.5 }
  ]
}*/
void main() {
    float aspect = RENDERSIZE.x / RENDERSIZE.y;
    vec2 p = (isf_FragNormCoord - 0.5) * vec2(aspect, 1.0);
    float u = dot(p, vec2(cos(angle), sin(angle))) * count + TIME * speed;
    float f = fract(u);
    float e = 0.5 * fwidth(u) + 1e-4;
    float m = smoothstep(0.0, e, f) * (1.0 - smoothstep(width, width + e, f));
    vec4 c = IMG_THIS_PIXEL(inputImage);
    gl_FragColor = vec4(mix(c.rgb, stripe_color.rgb, m * opacity), c.a);
}
