/*{
  "DESCRIPTION": "Green night-vision goggles with grain and a round mask.",
  "CATEGORIES": ["Stylize"],
  "INPUTS": [
    { "NAME": "inputImage", "TYPE": "image" },
    { "NAME": "gain", "TYPE": "float", "MIN": 0.5, "MAX": 4.0, "DEFAULT": 1.8 },
    { "NAME": "grain", "TYPE": "float", "MIN": 0.0, "MAX": 0.5, "DEFAULT": 0.15 },
    { "NAME": "mask", "TYPE": "float", "MIN": 0.0, "MAX": 1.0, "DEFAULT": 0.4 }
  ]
}*/
float hash(vec2 p) {
    return fract(sin(dot(p, vec2(12.9898, 78.233))) * 43758.5453);
}
void main() {
    vec4 c = IMG_THIS_PIXEL(inputImage);
    float l = dot(c.rgb, vec3(0.299, 0.587, 0.114)) * gain;
    float n = hash(isf_FragNormCoord * RENDERSIZE + fract(TIME) * 100.0) - 0.5;
    l = clamp(l + n * grain, 0.0, 1.0);
    float d = length(isf_FragNormCoord - 0.5) * 1.4142136;
    float v = 1.0 - mask * smoothstep(0.6, 1.0, d);
    gl_FragColor = vec4(vec3(0.1, 1.0, 0.25) * l * v + vec3(0.0, 0.03, 0.0), c.a);
}
