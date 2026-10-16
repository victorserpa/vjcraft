/*{
  "DESCRIPTION": "Text animator: every character moves, fades, scales or spins on its own beat. '|' starts a new line.",
  "CATEGORIES": ["Generator"],
  "INPUTS": [
    { "NAME": "message", "TYPE": "text", "DEFAULT": "VJCRAFT" },
    { "NAME": "background", "TYPE": "color", "DEFAULT": [0.0, 0.0, 0.0, 1.0] },
    { "NAME": "ink", "TYPE": "color", "DEFAULT": [1.0, 1.0, 1.0, 1.0] },
    { "NAME": "size", "TYPE": "float", "MIN": 0.05, "MAX": 0.6, "DEFAULT": 0.14 },
    { "NAME": "align", "TYPE": "long", "VALUES": [0, 1, 2], "LABELS": ["Left", "Centre", "Right"], "DEFAULT": 1 },
    { "NAME": "preset", "TYPE": "long", "VALUES": [0, 1, 2, 3, 4], "LABELS": ["Wave", "Fade in", "Pop", "Drop", "Spin"], "DEFAULT": 0 },
    { "NAME": "speed", "TYPE": "float", "MIN": 0.0, "MAX": 4.0, "DEFAULT": 1.0 },
    { "NAME": "stagger", "TYPE": "float", "MIN": 0.0, "MAX": 0.5, "DEFAULT": 0.12 },
    { "NAME": "amount", "TYPE": "float", "MIN": 0.0, "MAX": 1.0, "DEFAULT": 0.5 },
    { "NAME": "rotation", "TYPE": "float", "MIN": -180.0, "MAX": 180.0, "DEFAULT": 0.0 }
  ]
}*/
const int FONT[128] = int[128](0, 0, 1556480, 0, 0, 0, 343956, 2687, 0, 0, 137667, 12518, 1402038, 10274, 49664, 0, 560640, 65, 565376, 28, 1019434, 5404, 1016840, 1032, 796672, 0, 132104, 1032, 1585152, 0, 133216, 388, 1206462, 8005, 2089216, 64, 1339586, 9033, 1205441, 6985, 297496, 2175, 1139367, 7365, 1205564, 6217, 161921, 389, 1205430, 6985, 1205382, 3881, 891648, 0, 0, 0, 559624, 65, 330260, 2580, 565376, 1044, 1327234, 777, 1532094, 3925, 148734, 16137, 1205503, 6985, 1073342, 4417, 1073407, 8001, 1205503, 8393, 148735, 137, 1204414, 15689, 132223, 16264, 2089088, 65, 1073184, 191, 328831, 8354, 1056895, 8256, 196991, 16258, 65919, 16264, 1073342, 8001, 148735, 777, 1335486, 12065, 410879, 9001, 1205446, 6345, 2080897, 129, 1056831, 8128, 1052703, 4000, 397439, 16288, 133731, 12692, 1966595, 388, 1206497, 8645, 0, 0, 0, 0, 0, 0, 0, 0, 1056832, 8256);

// Raw character code at index i of the message (4 per uniform slot); lower case folds up.
int raw_at(int i) {
    if (i < 0 || i >= 24) { return 32; }
    float v = isf_u.p[message + i / 4][i - (i / 4) * 4];
    int c = int(v + 0.5);
    if (c >= 97 && c <= 122) { c -= 32; }
    return c;
}

bool glyph_on(int c, int cx, int cy) {
    if (c < 32 || c > 95) { return false; }
    int g = (c - 32) * 2;
    int bits;
    int col = cx;
    if (cx < 3) { bits = FONT[g]; } else { bits = FONT[g + 1]; col = cx - 3; }
    int shift = col * 7 + cy;
    return ((bits >> shift) & 1) == 1;
}

// Characters on the line that starts at message index `start` (a '|' ends a line).
int line_len(int start) {
    int n = 0;
    for (int i = 0; i < 24; i++) {
        if (i < start) { continue; }
        int c = raw_at(i);
        if (c == 124) { break; }
        n += 1;
    }
    return n;
}

int widest() {
    int best = 0;
    int cur = 0;
    for (int i = 0; i < 24; i++) {
        if (raw_at(i) == 124) { cur = 0; } else { cur += 1; best = max(best, cur); }
    }
    return best;
}

int line_count() {
    int n = 1;
    for (int i = 0; i < 24; i++) {
        if (raw_at(i) == 124) { n += 1; }
    }
    return n;
}

void main() {
    float aspect = RENDERSIZE.x / RENDERSIZE.y;
    vec2 p = (isf_FragNormCoord - 0.5) * vec2(aspect, 1.0);
    float ang = radians(rotation);
    float cs = cos(ang);
    float sn = sin(ang);
    vec2 q = vec2(cs * p.x - sn * p.y, sn * p.x + cs * p.y);
    float cell = size / 8.0;
    float pitch = size * 1.2;
    int lines = line_count();
    float top = (float(lines) * pitch - size * 0.2) * 0.5;
    float box = float(widest()) * 6.0 * cell;
    float cover = 0.0;
    int line = 0;
    int col = 0;
    float lw = 0.0;
    for (int i = 0; i < 24; i++) {
        int c = raw_at(i);
        if (c == 124) { line += 1; col = 0; continue; }
        if (col == 0) { lw = float(line_len(i)) * 6.0 * cell; }
        // Base centre of this character cell (5 columns of ink in a 6 wide advance).
        float x0 = -box * 0.5 + (box - lw) * float(align) * 0.5;
        vec2 bc = vec2(x0 + (float(col) * 6.0 + 2.5) * cell, top - float(line) * pitch - size * 0.5);
        col += 1;
        float ph = fract(TIME * speed - float(i) * stagger);
        float wave = sin(6.2831853 * ph);
        vec2 off = vec2(0.0);
        float sc = 1.0;
        float op = 1.0;
        float spin = 0.0;
        if (preset == 0) {
            off.y = wave * amount * size * 0.6;
        } else if (preset == 1) {
            op = smoothstep(0.0, 0.5, ph) * (1.0 - smoothstep(0.85, 1.0, ph));
        } else if (preset == 2) {
            sc = 1.0 + amount * sin(3.1415927 * ph) * 1.2;
        } else if (preset == 3) {
            float f = smoothstep(0.0, 0.5, ph);
            off.y = (1.0 - f) * amount * size * 3.0;
            op = f * (1.0 - smoothstep(0.85, 1.0, ph));
        } else {
            spin = ph * 6.2831853 * amount;
        }
        vec2 d = (q - bc - off) / max(sc, 0.01);
        float cd = cos(spin);
        float sd = sin(spin);
        d = vec2(cd * d.x + sd * d.y, -sd * d.x + cd * d.y) + vec2(2.5 * cell, -0.0);
        // d now lives in the character cell: x from the left ink column, y from the middle.
        float gx = d.x / cell;
        float gy = (size * 0.5 - d.y) / cell;
        if (gx >= 0.0 && gy >= 0.0) {
            int cx = int(floor(gx));
            int cy = int(floor(gy));
            if (cx < 5 && cy < 7) {
                if (glyph_on(c, cx, cy)) { cover = max(cover, op); }
            }
        }
    }
    gl_FragColor = mix(background, ink, cover);
}
