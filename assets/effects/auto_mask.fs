/*{
  "DESCRIPTION": "Masks the layer by its own brightness: pixels darker than the threshold turn transparent.",
  "CATEGORIES": ["Mask"],
  "INPUTS": [
    { "NAME": "inputImage", "TYPE": "image" },
    { "NAME": "threshold", "TYPE": "float", "MIN": 0.0, "MAX": 1.0, "DEFAULT": 0.5 },
    { "NAME": "feather", "TYPE": "float", "MIN": 0.0, "MAX": 1.0, "DEFAULT": 0.1 },
    { "NAME": "invert", "TYPE": "bool", "DEFAULT": false }
  ]
}*/
void main() {
    vec4 c = IMG_THIS_PIXEL(inputImage);
    float l = dot(c.rgb, vec3(0.2126, 0.7152, 0.0722));
    float f = max(feather * 0.5, 0.0005);
    float m = smoothstep(threshold - f, threshold + f, l);
    if (invert) { m = 1.0 - m; }
    gl_FragColor = vec4(c.rgb, c.a * m);
}
