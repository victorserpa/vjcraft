/*{
  "DESCRIPTION": "Boosts muted colours more than already saturated ones.",
  "CATEGORIES": ["Color"],
  "INPUTS": [
    { "NAME": "inputImage", "TYPE": "image" },
    { "NAME": "amount", "TYPE": "float", "MIN": -1.0, "MAX": 2.0, "DEFAULT": 0.8 }
  ]
}*/
void main() {
    vec4 c = IMG_THIS_PIXEL(inputImage);
    float l = dot(c.rgb, vec3(0.299, 0.587, 0.114));
    float mx = max(c.r, max(c.g, c.b));
    float mn = min(c.r, min(c.g, c.b));
    float sat = mx - mn;
    float k = 1.0 + amount * (1.0 - sat);
    gl_FragColor = vec4(clamp(mix(vec3(l), c.rgb, k), 0.0, 1.0), c.a);
}
