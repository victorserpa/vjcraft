/*{
  "DESCRIPTION": "Animated monochrome grain, strongest in the midtones.",
  "CATEGORIES": ["Stylize"],
  "INPUTS": [
    { "NAME": "inputImage", "TYPE": "image" },
    { "NAME": "amount", "TYPE": "float", "MIN": 0.0, "MAX": 1.0, "DEFAULT": 0.25 },
    { "NAME": "grain_size", "TYPE": "float", "MIN": 1.0, "MAX": 8.0, "DEFAULT": 1.5 },
    { "NAME": "fps", "TYPE": "float", "MIN": 1.0, "MAX": 60.0, "DEFAULT": 24.0 }
  ]
}*/
float hash(vec2 p) {
    return fract(sin(dot(p, vec2(127.1, 311.7))) * 43758.5453);
}
void main() {
    vec4 c = IMG_THIS_PIXEL(inputImage);
    vec2 cell = floor(isf_FragNormCoord * RENDERSIZE / grain_size);
    float n = hash(cell + vec2(floor(TIME * fps) * 13.7, 5.0)) - 0.5;
    float l = dot(c.rgb, vec3(0.2126, 0.7152, 0.0722));
    float mid = 1.0 - abs(l * 2.0 - 1.0) * 0.6;
    gl_FragColor = vec4(clamp(c.rgb + n * amount * mid, 0.0, 1.0), c.a);
}
