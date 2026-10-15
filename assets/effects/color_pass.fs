/*{
  "DESCRIPTION": "Keeps one hue and turns everything else grey.",
  "CATEGORIES": ["Color"],
  "INPUTS": [
    { "NAME": "inputImage", "TYPE": "image" },
    { "NAME": "hue", "TYPE": "float", "MIN": 0.0, "MAX": 1.0, "DEFAULT": 0.0 },
    { "NAME": "range", "TYPE": "float", "MIN": 0.01, "MAX": 0.5, "DEFAULT": 0.1 },
    { "NAME": "softness", "TYPE": "float", "MIN": 0.0, "MAX": 0.5, "DEFAULT": 0.08 },
    { "NAME": "grey_level", "TYPE": "float", "MIN": 0.0, "MAX": 1.0, "DEFAULT": 1.0 }
  ]
}*/
vec3 rgb2hsv_(vec3 c) {
    vec4 k = vec4(0.0, -1.0 / 3.0, 2.0 / 3.0, -1.0);
    vec4 p = mix(vec4(c.bg, k.wz), vec4(c.gb, k.xy), step(c.b, c.g));
    vec4 q = mix(vec4(p.xyw, c.r), vec4(c.r, p.yzx), step(p.x, c.r));
    float d = q.x - min(q.w, q.y);
    return vec3(abs(q.z + (q.w - q.y) / (6.0 * d + 1e-5)), d / (q.x + 1e-5), q.x);
}
void main() {
    vec4 c = IMG_THIS_PIXEL(inputImage);
    vec3 hsv = rgb2hsv_(c.rgb);
    float d = abs(hsv.x - hue);
    d = min(d, 1.0 - d);
    float keep = 1.0 - smoothstep(range, range + softness + 1e-4, d);
    keep *= smoothstep(0.02, 0.15, hsv.y);
    float l = dot(c.rgb, vec3(0.299, 0.587, 0.114));
    vec3 grey = vec3(l) * grey_level + c.rgb * 0.0;
    gl_FragColor = vec4(mix(grey, c.rgb, keep), c.a);
}
