/*{
  "DESCRIPTION": "3x3 matrix that rebuilds each output channel from the input channels.",
  "CATEGORIES": ["Color"],
  "INPUTS": [
    { "NAME": "inputImage", "TYPE": "image" },
    { "NAME": "r_from_r", "TYPE": "float", "MIN": -2.0, "MAX": 2.0, "DEFAULT": 1.0 },
    { "NAME": "r_from_g", "TYPE": "float", "MIN": -2.0, "MAX": 2.0, "DEFAULT": 0.0 },
    { "NAME": "r_from_b", "TYPE": "float", "MIN": -2.0, "MAX": 2.0, "DEFAULT": 0.0 },
    { "NAME": "g_from_r", "TYPE": "float", "MIN": -2.0, "MAX": 2.0, "DEFAULT": 0.0 },
    { "NAME": "g_from_g", "TYPE": "float", "MIN": -2.0, "MAX": 2.0, "DEFAULT": 1.0 },
    { "NAME": "g_from_b", "TYPE": "float", "MIN": -2.0, "MAX": 2.0, "DEFAULT": 0.0 },
    { "NAME": "b_from_r", "TYPE": "float", "MIN": -2.0, "MAX": 2.0, "DEFAULT": 0.0 },
    { "NAME": "b_from_g", "TYPE": "float", "MIN": -2.0, "MAX": 2.0, "DEFAULT": 0.0 },
    { "NAME": "b_from_b", "TYPE": "float", "MIN": -2.0, "MAX": 2.0, "DEFAULT": 1.0 }
  ]
}*/
void main() {
    vec4 c = IMG_THIS_PIXEL(inputImage);
    vec3 o = vec3(
        dot(c.rgb, vec3(r_from_r, r_from_g, r_from_b)),
        dot(c.rgb, vec3(g_from_r, g_from_g, g_from_b)),
        dot(c.rgb, vec3(b_from_r, b_from_g, b_from_b)));
    gl_FragColor = vec4(clamp(o, 0.0, 1.0), c.a);
}
