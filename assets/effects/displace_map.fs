/*{
  "DESCRIPTION": "Pushes pixels along a direction by their own brightness.",
  "CATEGORIES": ["Distort"],
  "INPUTS": [
    { "NAME": "inputImage", "TYPE": "image" },
    { "NAME": "amount", "TYPE": "float", "MIN": -0.3, "MAX": 0.3, "DEFAULT": 0.06 },
    { "NAME": "angle", "TYPE": "float", "MIN": 0.0, "MAX": 6.2831853, "DEFAULT": 0.785 },
    { "NAME": "midpoint", "TYPE": "float", "MIN": 0.0, "MAX": 1.0, "DEFAULT": 0.5 },
    { "NAME": "pre_blur", "TYPE": "float", "MIN": 0.0, "MAX": 10.0, "DEFAULT": 2.0 }
  ]
}*/
void main() {
    vec2 uv = isf_FragNormCoord;
    vec2 px = pre_blur / RENDERSIZE;
    vec3 k = vec3(0.299, 0.587, 0.114);
    float l = dot(IMG_NORM_PIXEL(inputImage, uv).rgb, k) * 0.4;
    l += dot(IMG_NORM_PIXEL(inputImage, uv + vec2(px.x, 0.0)).rgb, k) * 0.15;
    l += dot(IMG_NORM_PIXEL(inputImage, uv - vec2(px.x, 0.0)).rgb, k) * 0.15;
    l += dot(IMG_NORM_PIXEL(inputImage, uv + vec2(0.0, px.y)).rgb, k) * 0.15;
    l += dot(IMG_NORM_PIXEL(inputImage, uv - vec2(0.0, px.y)).rgb, k) * 0.15;
    vec2 dir = vec2(cos(angle), sin(angle));
    gl_FragColor = IMG_NORM_PIXEL(inputImage, uv + dir * (l - midpoint) * amount);
}
