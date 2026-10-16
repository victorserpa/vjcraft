/*{
  "DESCRIPTION": "Masks the layer with the brightness or the alpha of an image file, stretched to the frame.",
  "CATEGORIES": ["Mask"],
  "INPUTS": [
    { "NAME": "inputImage", "TYPE": "image" },
    { "NAME": "channel", "TYPE": "long", "VALUES": [0, 1], "LABELS": ["Luma", "Alpha"], "DEFAULT": 0 },
    { "NAME": "invert", "TYPE": "bool", "DEFAULT": false },
    { "NAME": "file", "TYPE": "text", "DEFAULT": "" }
  ]
}*/
// The image arrives in the spare texture slot (see FxLib::still).
void main() {
    vec4 c = IMG_THIS_PIXEL(inputImage);
    vec4 k = IMG_THIS_PIXEL(vjc_mask);
    float m = channel == 0 ? dot(k.rgb, vec3(0.2126, 0.7152, 0.0722)) * k.a : k.a;
    if (invert) { m = 1.0 - m; }
    gl_FragColor = vec4(c.rgb, c.a * m);
}
