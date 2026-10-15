/*{
  "DESCRIPTION": "Drops an offset, softened shadow behind the picture, cast by its alpha or by its brightness.",
  "CATEGORIES": ["Stylize"],
  "INPUTS": [
    { "NAME": "inputImage", "TYPE": "image" },
    { "NAME": "shadow_color", "TYPE": "color", "DEFAULT": [0.0, 0.0, 0.0, 1.0] },
    { "NAME": "offset_x", "TYPE": "float", "MIN": -0.2, "MAX": 0.2, "DEFAULT": 0.03 },
    { "NAME": "offset_y", "TYPE": "float", "MIN": -0.2, "MAX": 0.2, "DEFAULT": -0.03 },
    { "NAME": "softness", "TYPE": "float", "MIN": 0.0, "MAX": 0.05, "DEFAULT": 0.01 },
    { "NAME": "opacity", "TYPE": "float", "MIN": 0.0, "MAX": 1.0, "DEFAULT": 0.7 },
    { "NAME": "from_luma", "TYPE": "bool", "DEFAULT": false }
  ]
}*/
float cover_(vec2 uv) {
    vec4 c = IMG_NORM_PIXEL(inputImage, uv);
    return from_luma ? dot(c.rgb, vec3(0.299, 0.587, 0.114)) : c.a;
}
void main() {
    vec2 uv = isf_FragNormCoord;
    vec2 o = vec2(offset_x, offset_y);
    float s = cover_(uv - o) * 0.28;
    s += cover_(uv - o + vec2(softness, 0.0)) * 0.18;
    s += cover_(uv - o - vec2(softness, 0.0)) * 0.18;
    s += cover_(uv - o + vec2(0.0, softness)) * 0.18;
    s += cover_(uv - o - vec2(0.0, softness)) * 0.18;
    vec4 c = IMG_THIS_PIXEL(inputImage);
    float here = from_luma ? dot(c.rgb, vec3(0.299, 0.587, 0.114)) : c.a;
    float sh = s * (1.0 - here) * opacity * shadow_color.a;
    vec3 rgb = from_luma ? mix(c.rgb, shadow_color.rgb, sh) : mix(shadow_color.rgb, c.rgb, c.a);
    float a = from_luma ? c.a : max(c.a, sh);
    if (!from_luma && a < 1e-4) {
        rgb = shadow_color.rgb;
    }
    gl_FragColor = vec4(rgb, a);
}
