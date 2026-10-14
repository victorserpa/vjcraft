/*{
  "DESCRIPTION": "Sine-product dot pattern multiplied into the picture, like a coarse print screen.",
  "CATEGORIES": ["Stylize"],
  "INPUTS": [
    { "NAME": "inputImage", "TYPE": "image" },
    { "NAME": "scale", "TYPE": "float", "MIN": 0.1, "MAX": 3.0, "DEFAULT": 0.7 },
    { "NAME": "angle", "TYPE": "float", "MIN": 0.0, "MAX": 6.2831853, "DEFAULT": 1.57 },
    { "NAME": "sharpness", "TYPE": "float", "MIN": 1.0, "MAX": 10.0, "DEFAULT": 4.0 }
  ]
}*/
void main() {
    vec4 c = IMG_THIS_PIXEL(inputImage);
    vec2 px = isf_FragNormCoord * RENDERSIZE;
    float cs = cos(angle);
    float sn = sin(angle);
    vec2 r = vec2(px.x * cs - px.y * sn, px.x * sn + px.y * cs) * scale;
    float pat = (sin(r.x) * sin(r.y)) * sharpness * 0.25;
    vec3 v = c.rgb * 3.0 - 1.0 + vec3(pat);
    gl_FragColor = vec4(clamp(v, 0.0, 1.0), c.a);
}
