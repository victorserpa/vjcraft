/*{
  "DESCRIPTION": "Gamma correction with a separate value for red, green and blue.",
  "CATEGORIES": ["Color"],
  "INPUTS": [
    { "NAME": "inputImage", "TYPE": "image" },
    { "NAME": "gamma_r", "TYPE": "float", "MIN": 0.2, "MAX": 4.0, "DEFAULT": 1.0 },
    { "NAME": "gamma_g", "TYPE": "float", "MIN": 0.2, "MAX": 4.0, "DEFAULT": 1.0 },
    { "NAME": "gamma_b", "TYPE": "float", "MIN": 0.2, "MAX": 4.0, "DEFAULT": 1.0 }
  ]
}*/
void main() {
    vec4 c = IMG_THIS_PIXEL(inputImage);
    vec3 g = vec3(gamma_r, gamma_g, gamma_b);
    gl_FragColor = vec4(pow(max(c.rgb, vec3(0.0)), 1.0 / g), c.a);
}
