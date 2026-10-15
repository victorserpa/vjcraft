/*{
  "DESCRIPTION": "Thermal camera false colour from luminance.",
  "CATEGORIES": ["Stylize"],
  "INPUTS": [
    { "NAME": "inputImage", "TYPE": "image" },
    { "NAME": "contrast", "TYPE": "float", "MIN": 0.2, "MAX": 3.0, "DEFAULT": 1.0 },
    { "NAME": "offset", "TYPE": "float", "MIN": -0.5, "MAX": 0.5, "DEFAULT": 0.0 }
  ]
}*/
void main() {
    vec4 c = IMG_THIS_PIXEL(inputImage);
    float l = clamp((dot(c.rgb, vec3(0.299, 0.587, 0.114)) - 0.5) * contrast + 0.5 + offset, 0.0, 1.0);
    vec3 cold = vec3(0.0, 0.0, 0.35);
    vec3 a = mix(cold, vec3(0.5, 0.0, 0.6), smoothstep(0.0, 0.25, l));
    vec3 b = mix(a, vec3(0.9, 0.1, 0.1), smoothstep(0.25, 0.5, l));
    vec3 d = mix(b, vec3(1.0, 0.8, 0.1), smoothstep(0.5, 0.75, l));
    vec3 e = mix(d, vec3(1.0, 1.0, 0.9), smoothstep(0.75, 1.0, l));
    gl_FragColor = vec4(e, c.a);
}
