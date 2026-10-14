/*{
  "DESCRIPTION": "Maps brightness onto a blend between two colours.",
  "CATEGORIES": ["Color"],
  "INPUTS": [
    { "NAME": "inputImage", "TYPE": "image" },
    { "NAME": "shadow", "TYPE": "color", "DEFAULT": [0.08, 0.04, 0.3, 1.0] },
    { "NAME": "highlight", "TYPE": "color", "DEFAULT": [1.0, 0.75, 0.3, 1.0] },
    { "NAME": "contrast", "TYPE": "float", "MIN": 0.2, "MAX": 3.0, "DEFAULT": 1.0 }
  ]
}*/
void main() {
    vec4 c = IMG_THIS_PIXEL(inputImage);
    float l = dot(c.rgb, vec3(0.2126, 0.7152, 0.0722));
    l = clamp((l - 0.5) * contrast + 0.5, 0.0, 1.0);
    gl_FragColor = vec4(mix(shadow.rgb, highlight.rgb, l), c.a);
}
