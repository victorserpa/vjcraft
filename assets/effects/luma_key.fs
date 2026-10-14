/*{
  "DESCRIPTION": "Makes pixels transparent by brightness range (darks, or brights when inverted).",
  "CATEGORIES": ["Keying"],
  "INPUTS": [
    { "NAME": "inputImage", "TYPE": "image" },
    { "NAME": "cutoff", "TYPE": "float", "MIN": 0.0, "MAX": 1.0, "DEFAULT": 0.2 },
    { "NAME": "softness", "TYPE": "float", "MIN": 0.0, "MAX": 0.5, "DEFAULT": 0.1 },
    { "NAME": "invert", "TYPE": "bool", "DEFAULT": false }
  ]
}*/
void main() {
    vec4 c = IMG_THIS_PIXEL(inputImage);
    float l = dot(c.rgb, vec3(0.2126, 0.7152, 0.0722));
    float k = smoothstep(cutoff - softness, cutoff + softness + 0.0005, l);
    if (invert) { k = 1.0 - k; }
    gl_FragColor = vec4(c.rgb, c.a * k);
}
