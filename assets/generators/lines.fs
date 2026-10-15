/*{
  "DESCRIPTION": "Scrolling parallel lines at any angle.",
  "CATEGORIES": ["Generator"],
  "INPUTS": [
    { "NAME": "background", "TYPE": "color", "DEFAULT": [0.02, 0.02, 0.08, 1.0] },
    { "NAME": "color", "TYPE": "color", "DEFAULT": [0.9, 0.95, 1.0, 1.0] },
    { "NAME": "count", "TYPE": "float", "MIN": 1.0, "MAX": 100.0, "DEFAULT": 16.0 },
    { "NAME": "thickness", "TYPE": "float", "MIN": 0.02, "MAX": 0.95, "DEFAULT": 0.3 },
    { "NAME": "angle", "TYPE": "float", "MIN": -180.0, "MAX": 180.0, "DEFAULT": 45.0 },
    { "NAME": "speed", "TYPE": "float", "MIN": -4.0, "MAX": 4.0, "DEFAULT": 0.5 }
  ]
}*/
void main() {
    float aspect = RENDERSIZE.x / RENDERSIZE.y;
    vec2 p = (isf_FragNormCoord - 0.5) * vec2(aspect, 1.0);
    float a = radians(angle);
    float u = p.x * cos(a) + p.y * sin(a);
    float f = fract(u * count + TIME * speed);
    float aa = fwidth(u * count) + 0.001;
    float m = smoothstep(0.0, aa, f) * (1.0 - smoothstep(thickness, thickness + aa, f));
    gl_FragColor = vec4(mix(background.rgb, color.rgb, m), 1.0);
}
