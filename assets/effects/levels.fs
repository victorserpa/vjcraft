/*{
  "DESCRIPTION": "Input and output black/white points with a midtone gamma.",
  "CATEGORIES": ["Color"],
  "INPUTS": [
    { "NAME": "inputImage", "TYPE": "image" },
    { "NAME": "in_black", "TYPE": "float", "MIN": 0.0, "MAX": 0.95, "DEFAULT": 0.05 },
    { "NAME": "in_white", "TYPE": "float", "MIN": 0.05, "MAX": 1.0, "DEFAULT": 0.95 },
    { "NAME": "gamma", "TYPE": "float", "MIN": 0.2, "MAX": 3.0, "DEFAULT": 1.1 },
    { "NAME": "out_black", "TYPE": "float", "MIN": 0.0, "MAX": 1.0, "DEFAULT": 0.0 },
    { "NAME": "out_white", "TYPE": "float", "MIN": 0.0, "MAX": 1.0, "DEFAULT": 1.0 }
  ]
}*/
void main() {
    vec4 c = IMG_THIS_PIXEL(inputImage);
    float span = max(in_white - in_black, 0.001);
    vec3 t = clamp((c.rgb - vec3(in_black)) / span, 0.0, 1.0);
    t = pow(t, vec3(1.0 / max(gamma, 0.01)));
    gl_FragColor = vec4(mix(vec3(out_black), vec3(out_white), t), c.a);
}
