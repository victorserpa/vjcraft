/*{
  "DESCRIPTION": "Colour grade with a .cube 3D LUT file.",
  "CATEGORIES": ["Color"],
  "INPUTS": [
    { "NAME": "inputImage", "TYPE": "image" },
    { "NAME": "strength", "TYPE": "float", "MIN": 0.0, "MAX": 1.0, "DEFAULT": 1.0 },
    { "NAME": "file", "TYPE": "text", "DEFAULT": "" }
  ]
}*/
// The LUT arrives as a strip texture in the spare texture slot (see vjc_fx::lut).
vec3 lut_at(float slice, vec3 c, float n) {
    vec2 px = vec2(slice * n + c.r * (n - 1.0) + 0.5, c.g * (n - 1.0) + 0.5);
    return textureLod(sampler2D(vjc_mask, isf_smp), px / vec2(n * n, n), 0.0).rgb;
}
void main() {
    vec4 c = IMG_THIS_PIXEL(inputImage);
    float n = float(textureSize(sampler2D(vjc_mask, isf_smp), 0).y);
    vec3 x = clamp(c.rgb, 0.0, 1.0);
    float z = x.b * (n - 1.0);
    float z0 = floor(z);
    float z1 = min(z0 + 1.0, n - 1.0);
    vec3 o = mix(lut_at(z0, x, n), lut_at(z1, x, n), z - z0);
    gl_FragColor = vec4(mix(c.rgb, o, strength), c.a);
}
