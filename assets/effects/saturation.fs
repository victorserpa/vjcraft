/*{
  "DESCRIPTION": "Scales colour intensity from grayscale up to oversaturated.",
  "CATEGORIES": ["Color"],
  "INPUTS": [
    { "NAME": "inputImage", "TYPE": "image" },
    { "NAME": "amount", "TYPE": "float", "MIN": 0.0, "MAX": 3.0, "DEFAULT": 1.5 }
  ]
}*/
void main() {
    vec4 c = IMG_THIS_PIXEL(inputImage);
    float l = dot(c.rgb, vec3(0.2126, 0.7152, 0.0722));
    gl_FragColor = vec4(clamp(mix(vec3(l), c.rgb, amount), 0.0, 1.0), c.a);
}
