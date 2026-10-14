/*{
  "DESCRIPTION": "Video feedback: the previous frame is zoomed, rotated and mixed back in.",
  "CATEGORIES": ["Feedback"],
  "PERSISTENT": true,
  "INPUTS": [
    { "NAME": "inputImage", "TYPE": "image" },
    { "NAME": "feedback", "TYPE": "float", "MIN": 0.0, "MAX": 0.95, "DEFAULT": 0.6 },
    { "NAME": "zoom", "TYPE": "float", "MIN": 0.9, "MAX": 1.1, "DEFAULT": 1.01 },
    { "NAME": "twist", "TYPE": "float", "MIN": -0.1, "MAX": 0.1, "DEFAULT": 0.01 }
  ]
}*/
void main() {
    vec2 c = isf_FragNormCoord - 0.5;
    c.x *= RENDERSIZE.x / RENDERSIZE.y;
    float s = sin(twist);
    float k = cos(twist);
    c = vec2(k * c.x - s * c.y, s * c.x + k * c.y) / zoom;
    c.x /= RENDERSIZE.x / RENDERSIZE.y;
    vec4 old = PREV_PIXEL(c + 0.5);
    vec4 now = IMG_THIS_PIXEL(inputImage);
    gl_FragColor = mix(now, max(now, old), feedback);
}
