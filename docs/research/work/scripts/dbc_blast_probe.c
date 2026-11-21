#include <limits.h>
#include <stddef.h>
#include <stdint.h>
#include <string.h>

#include "blast.h"

struct probe_output {
    unsigned char *buffer;
    size_t length;
    size_t capacity;
};

static unsigned no_input(void *context, unsigned char **buffer)
{
    (void)context;
    (void)buffer;
    return 0;
}

static int collect_output(void *context, unsigned char *buffer, unsigned length)
{
    struct probe_output *output = context;
    if (length > output->capacity - output->length)
        return 1;
    memcpy(output->buffer + output->length, buffer, length);
    output->length += length;
    return 0;
}

/* Decode bounded output and report how many whole compressed bytes remain. */
int probe_dcl(const unsigned char *input, size_t input_length,
              unsigned char *output, size_t capacity,
              size_t *output_length, size_t *unused_bytes)
{
    if (input_length > UINT_MAX)
        return 2;
    unsigned left = (unsigned)input_length;
    unsigned char *next = (unsigned char *)input;
    struct probe_output state = {output, 0, capacity};
    int status = blast(no_input, NULL, collect_output, &state, &left, &next);
    *unused_bytes = left;
    *output_length = state.length;
    return status;
}

/* Compute a CRC-32 register before applying the caller's final XOR. */
uint32_t probe_crc_register(const unsigned char *data, size_t length,
                            uint32_t polynomial, uint32_t initial, int reflected)
{
    uint32_t table[256];
    for (unsigned index = 0; index < 256; index++) {
        uint32_t value = reflected ? index : (uint32_t)index << 24;
        for (unsigned bit = 0; bit < 8; bit++) {
            if (reflected)
                value = (value >> 1) ^ ((value & 1) ? polynomial : 0);
            else
                value = (value << 1) ^ ((value >> 31) ? polynomial : 0);
        }
        table[index] = value;
    }

    uint32_t crc = initial;
    if (reflected) {
        for (size_t index = 0; index < length; index++)
            crc = (crc >> 8) ^ table[(crc ^ data[index]) & 0xff];
    } else {
        for (size_t index = 0; index < length; index++)
            crc = (crc << 8) ^ table[(crc >> 24) ^ data[index]];
    }
    return crc;
}
