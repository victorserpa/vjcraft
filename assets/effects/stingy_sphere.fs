/*{
  "DESCRIPTION": "A glass sphere that refracts the picture, with a pinched, upside-down look at high strength.",
  "CATEGORIES": ["Distort"],
  "INPUTS": [
    { "NAME": "inputImage", "TYPE": "image" },
    { "NAME": "center", "TYPE": "point2D", "MIN": [0.0, 0.0], "MAX": [1.0, 1.0], "DEFAULT": [0.5, 0.5] },
    { "NAME": "radius", "TYPE": "float", "MIN": 0.05, "MAX": 0.8, "DEFAULT": 0.35 },
    { "NAME": "strength", "TYPE": "float", "MIN": 0.0, "MAX": 2.0, "DEFAULT": 1.0 },
    { "NAME": "rim", "TYPE": "float", "MIN": 0.0, "MAX": 1.0, "DEFAULT": 0.3 }
  ]
}*/
void main() {
    float aspect = RENDERSIZE.x / RENDERSIZE.y;
    vec2 uv = isf_FragNormCoord;
    vec2 p = (uv - center) * vec2(aspect, 1.0);
    float d = length(p) / radius;
    vec4 c = IMG_NORM_PIXEL(inputImage, uv);
    if (d < 1.0) {
        float z = sqrt(max(1.0 - d * d, 0.0));
        // Refraction: rays bend toward the axis; stronger toward the middle of the ball.
        vec2 n = p / radius;
        vec2 q = p - n * radius * z * strength * 0.9;
        q.x /= aspect;
        c = IMG_NORM_PIXEL(inputImage, q + center);
        float edge = pow(d, 4.0);
        c.rgb = mix(c.rgb, vec3(1.0), edge * rim * 0.5);
        c.rgb *= 1.0 - 0.25 * edge * rim;
    }
    gl_FragColor = c;
}
