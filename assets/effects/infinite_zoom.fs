/*{
  "DESCRIPTION": "Endless zoom tunnel: layers of the image keep scaling into the centre.",
  "CATEGORIES": ["Time"],
  "INPUTS": [
    { "NAME": "inputImage", "TYPE": "image" },
    { "NAME": "speed", "TYPE": "float", "MIN": -2.0, "MAX": 2.0, "DEFAULT": 0.3 },
    { "NAME": "scale", "TYPE": "float", "MIN": 1.2, "MAX": 4.0, "DEFAULT": 2.0 },
    { "NAME": "twist", "TYPE": "float", "MIN": -3.0, "MAX": 3.0, "DEFAULT": 0.0 }
  ]
}*/
void main() {
    vec2 p = isf_FragNormCoord - 0.5;
    float t = fract(TIME * speed);
    float z1 = pow(scale, t);
    float z2 = z1 / scale;
    float a = twist * t;
    float s = sin(a);
    float c = cos(a);
    vec2 q = vec2(c * p.x - s * p.y, s * p.x + c * p.y);
    vec4 a1 = IMG_NORM_PIXEL(inputImage, 0.5 + q / z1);
    vec4 a2 = IMG_NORM_PIXEL(inputImage, 0.5 + q / z2);
    gl_FragColor = mix(a1, a2, t);
}
