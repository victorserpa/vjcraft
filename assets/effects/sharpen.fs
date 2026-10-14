/*{
  "DESCRIPTION": "Unsharp mask: adds back the difference to a local average.",
  "CATEGORIES": ["Blur"],
  "INPUTS": [
    { "NAME": "inputImage", "TYPE": "image" },
    { "NAME": "amount", "TYPE": "float", "MIN": 0.0, "MAX": 4.0, "DEFAULT": 1.0 },
    { "NAME": "radius", "TYPE": "float", "MIN": 0.5, "MAX": 6.0, "DEFAULT": 1.5 }
  ]
}*/
void main() {
    vec2 px = radius / RENDERSIZE;
    vec4 c = IMG_THIS_PIXEL(inputImage);
    vec3 avg = IMG_NORM_PIXEL(inputImage, isf_FragNormCoord + vec2(px.x, 0.0)).rgb
        + IMG_NORM_PIXEL(inputImage, isf_FragNormCoord - vec2(px.x, 0.0)).rgb
        + IMG_NORM_PIXEL(inputImage, isf_FragNormCoord + vec2(0.0, px.y)).rgb
        + IMG_NORM_PIXEL(inputImage, isf_FragNormCoord - vec2(0.0, px.y)).rgb;
    avg *= 0.25;
    gl_FragColor = vec4(clamp(c.rgb + (c.rgb - avg) * amount, 0.0, 1.0), c.a);
}
