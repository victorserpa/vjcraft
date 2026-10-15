/*{
  "DESCRIPTION": "Warm brown vintage tone.",
  "CATEGORIES": ["Color"],
  "INPUTS": [
    { "NAME": "inputImage", "TYPE": "image" },
    { "NAME": "amount", "TYPE": "float", "MIN": 0.0, "MAX": 1.0, "DEFAULT": 1.0 }
  ]
}*/
void main() {
    vec4 c = IMG_THIS_PIXEL(inputImage);
    vec3 s = vec3(
        dot(c.rgb, vec3(0.393, 0.769, 0.189)),
        dot(c.rgb, vec3(0.349, 0.686, 0.168)),
        dot(c.rgb, vec3(0.272, 0.534, 0.131)));
    gl_FragColor = vec4(mix(c.rgb, clamp(s, 0.0, 1.0), amount), c.a);
}
