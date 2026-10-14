/*{
  "DESCRIPTION": "Relief look lit from an angle.",
  "CATEGORIES": ["Stylize"],
  "INPUTS": [
    { "NAME": "inputImage", "TYPE": "image" },
    { "NAME": "depth_amount", "TYPE": "float", "MIN": 0.0, "MAX": 6.0, "DEFAULT": 2.0 },
    { "NAME": "angle", "TYPE": "float", "MIN": 0.0, "MAX": 6.2831853, "DEFAULT": 0.785 },
    { "NAME": "color_mix", "TYPE": "float", "MIN": 0.0, "MAX": 1.0, "DEFAULT": 0.3 }
  ]
}*/
void main() {
    vec2 d = vec2(cos(angle), sin(angle)) / RENDERSIZE;
    vec4 c = IMG_THIS_PIXEL(inputImage);
    vec3 a = IMG_NORM_PIXEL(inputImage, isf_FragNormCoord + d).rgb;
    vec3 b = IMG_NORM_PIXEL(inputImage, isf_FragNormCoord - d).rgb;
    float diff = dot(a - b, vec3(0.3333)) * depth_amount;
    vec3 relief = vec3(0.5 + diff);
    gl_FragColor = vec4(clamp(mix(relief, relief * c.rgb * 2.0, color_mix), 0.0, 1.0), c.a);
}
