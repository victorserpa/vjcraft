/*{
  "DESCRIPTION": "Sine ripple that pushes pixels sideways or up and down.",
  "CATEGORIES": ["Distort"],
  "INPUTS": [
    { "NAME": "inputImage", "TYPE": "image" },
    { "NAME": "amplitude", "TYPE": "float", "MIN": 0.0, "MAX": 0.2, "DEFAULT": 0.02 },
    { "NAME": "frequency", "TYPE": "float", "MIN": 0.5, "MAX": 40.0, "DEFAULT": 8.0 },
    { "NAME": "speed", "TYPE": "float", "MIN": 0.0, "MAX": 8.0, "DEFAULT": 1.5 },
    { "NAME": "vertical", "TYPE": "bool", "DEFAULT": false }
  ]
}*/
void main() {
    vec2 uv = isf_FragNormCoord;
    if (vertical) {
        uv.y += amplitude * sin(uv.x * frequency + TIME * speed);
    } else {
        uv.x += amplitude * sin(uv.y * frequency + TIME * speed);
    }
    gl_FragColor = IMG_NORM_PIXEL(inputImage, uv);
}
