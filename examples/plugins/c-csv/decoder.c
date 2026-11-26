/* Example embdbg decoder plugin in C: CSV lines into channels.
 *
 * Build (no libc, no imports):
 *   clang --target=wasm32 -O2 -nostdlib \
 *     -Wl,--no-entry -Wl,--export-memory \
 *     -Wl,--export=input_ptr -Wl,--export=input_capacity -Wl,--export=decode \
 *     -o decoder.wasm decoder.c
 *
 * ABI: docs/plugins.md. The host writes one frame into `input`, then calls
 * decode(len), which returns a pointer to [u32 length][u8 kind][payload].
 */

#define INPUT_LEN 4096
#define OUTPUT_LEN 4096

static unsigned char input[INPUT_LEN];
static unsigned char output[OUTPUT_LEN];

__attribute__((export_name("input_ptr"))) int input_ptr(void) {
    return (int)input;
}

__attribute__((export_name("input_capacity"))) int input_capacity(void) {
    return INPUT_LEN;
}

static int parse_float(const unsigned char *text, int len, float *value) {
    int i = 0, negative = 0, digits = 0;
    double number = 0.0, fraction = 0.1;
    while (i < len && (text[i] == ' ' || text[i] == '\t')) i++;
    if (i < len && (text[i] == '-' || text[i] == '+')) {
        negative = text[i] == '-';
        i++;
    }
    while (i < len && text[i] >= '0' && text[i] <= '9') {
        number = number * 10.0 + (text[i] - '0');
        digits++;
        i++;
    }
    if (i < len && text[i] == '.') {
        i++;
        while (i < len && text[i] >= '0' && text[i] <= '9') {
            number += (text[i] - '0') * fraction;
            fraction *= 0.1;
            digits++;
            i++;
        }
    }
    while (i < len && (text[i] == ' ' || text[i] == '\t' || text[i] == '\r')) i++;
    if (digits == 0 || i != len) return 0;
    *value = (float)(negative ? -number : number);
    return 1;
}

__attribute__((export_name("decode"))) int decode(int len) {
    int start = 0, count = 0, at = 5;
    float value;
    if (len <= 0 || len > INPUT_LEN) return 0;
    for (int i = 0; i <= len; i++) {
        int end = i == len || input[i] == ',';
        if (!end) continue;
        if (!parse_float(input + start, i - start, &value)) return 0;
        if (at + 4 > OUTPUT_LEN) return 0;
        unsigned int bits;
        __builtin_memcpy(&bits, &value, 4);
        output[at++] = (unsigned char)(bits & 0xFF);
        output[at++] = (unsigned char)((bits >> 8) & 0xFF);
        output[at++] = (unsigned char)((bits >> 16) & 0xFF);
        output[at++] = (unsigned char)((bits >> 24) & 0xFF);
        count++;
        start = i + 1;
    }
    if (count == 0) return 0;
    unsigned int payload = (unsigned int)(count * 4);
    output[0] = (unsigned char)(payload & 0xFF);
    output[1] = (unsigned char)((payload >> 8) & 0xFF);
    output[2] = (unsigned char)((payload >> 16) & 0xFF);
    output[3] = (unsigned char)((payload >> 24) & 0xFF);
    output[4] = 0; /* f32 samples */
    return (int)output;
}
