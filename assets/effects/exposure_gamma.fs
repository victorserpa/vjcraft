/*{
  "DESCRIPTION": "Exposure in stops plus a gamma curve.",
  "CATEGORIES": ["Color"],
  "INPUTS": [
    { "NAME": "inputImage", "TYPE": "image" },
    { "NAME": "exposure", "TYPE": "float", "MIN": -3.0, "MAX": 3.0, "DEFAULT": 0.0 },
    { "NAME": "gamma", "TYPE": "float", "MIN": 0.2, "MAX": 3.0, "DEFAULT": 1.0 }
  ]
}*/
void main() {
    vec4 c = IMG_THIS_PIXEL(inputImage);
    vec3 e = c.rgb * exp2(exposure);
    gl_FragColor = vec4(pow(clamp(e, 0.0, 1.0), vec3(1.0 / gamma)), c.a);
}
