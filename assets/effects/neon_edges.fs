/*{
  "DESCRIPTION": "Glowing coloured outlines over a darkened picture.",
  "CATEGORIES": ["Stylize"],
  "INPUTS": [
    { "NAME": "inputImage", "TYPE": "image" },
    { "NAME": "strength", "TYPE": "float", "MIN": 0.0, "MAX": 10.0, "DEFAULT": 3.0 },
    { "NAME": "dim", "TYPE": "float", "MIN": 0.0, "MAX": 1.0, "DEFAULT": 0.25 },
    { "NAME": "hue_cycle", "TYPE": "float", "MIN": 0.0, "MAX": 2.0, "DEFAULT": 0.3 }
  ]
}*/
vec3 hsv(float h) {
    return clamp(abs(fract(vec3(h) + vec3(0.0, 2.0 / 3.0, 1.0 / 3.0)) * 6.0 - 3.0) - 1.0, 0.0, 1.0);
}
void main() {
    vec2 px = vec2(1.0) / RENDERSIZE;
    vec2 u = isf_FragNormCoord;
    vec3 c = IMG_THIS_PIXEL(inputImage).rgb;
    vec3 l = IMG_NORM_PIXEL(inputImage, u - vec2(px.x, 0.0)).rgb;
    vec3 r = IMG_NORM_PIXEL(inputImage, u + vec2(px.x, 0.0)).rgb;
    vec3 t = IMG_NORM_PIXEL(inputImage, u + vec2(0.0, px.y)).rgb;
    vec3 b = IMG_NORM_PIXEL(inputImage, u - vec2(0.0, px.y)).rgb;
    vec3 e = abs(r - l) + abs(t - b);
    float m = clamp(dot(e, vec3(0.3333)) * strength, 0.0, 1.0);
    vec3 neon = hsv(dot(c, vec3(0.3333)) + TIME * hue_cycle) * m;
    gl_FragColor = vec4(c * dim + neon + e * strength * 0.2 * c, IMG_THIS_PIXEL(inputImage).a);
}
