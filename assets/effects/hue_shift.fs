/*{
  "DESCRIPTION": "Rotates the hue.",
  "CATEGORIES": ["Color"],
  "INPUTS": [
    { "NAME": "inputImage", "TYPE": "image" },
    { "NAME": "shift", "TYPE": "float", "MIN": 0.0, "MAX": 1.0, "DEFAULT": 0.25 }
  ]
}*/
void main() {
    vec4 c = IMG_THIS_PIXEL(inputImage);
    float a = shift * 6.2831853;
    float cs = cos(a);
    float sn = sin(a);
    // Rotation in YIQ space.
    float y = dot(c.rgb, vec3(0.299, 0.587, 0.114));
    float i = dot(c.rgb, vec3(0.596, -0.274, -0.322));
    float q = dot(c.rgb, vec3(0.211, -0.523, 0.312));
    float i2 = i * cs - q * sn;
    float q2 = i * sn + q * cs;
    vec3 rgb = vec3(
        y + 0.956 * i2 + 0.621 * q2,
        y - 0.272 * i2 - 0.647 * q2,
        y - 1.106 * i2 + 1.703 * q2);
    gl_FragColor = vec4(clamp(rgb, 0.0, 1.0), c.a);
}
