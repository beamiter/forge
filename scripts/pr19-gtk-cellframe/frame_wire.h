/* Fixture-private, bounded stdin reader; no production protocol. */
#ifndef FORGE_PR19_FRAME_WIRE_H
#define FORGE_PR19_FRAME_WIRE_H
#include <stdbool.h>
#include <stdio.h>
#include <string.h>

enum { MAX_COLS = 4, MAX_ROWS = 2, FRAME_COUNT = 2 };
typedef struct {
    unsigned red, green, blue, codepoint, width;
} PaintCell;
typedef struct {
    unsigned revision, cols, rows, cursor_row, cursor_col;
    PaintCell cells[MAX_ROWS][MAX_COLS];
} PaintFrame;

static bool read_frames(FILE *input, PaintFrame frames[FRAME_COUNT])
{
    char header[32];
    if (fgets(header, sizeof(header), input) == NULL || strcmp(header, "PR19_FRAME_V1\n"))
        return false;
    for (unsigned i = 0; i < FRAME_COUNT; ++i) {
        PaintFrame *f = &frames[i];
        if (fscanf(input, "%4u %4u %4u %4u %4u", &f->revision, &f->cols, &f->rows,
                   &f->cursor_row, &f->cursor_col) != 5 ||
            f->revision != i + 1 || f->cols == 0 || f->cols > MAX_COLS ||
            f->rows == 0 || f->rows > MAX_ROWS || f->cursor_row >= f->rows ||
            f->cursor_col >= f->cols)
            return false;
        for (unsigned row = 0; row < f->rows; ++row) {
            bool continuation = false;
            for (unsigned col = 0; col < f->cols; ++col) {
                PaintCell *c = &f->cells[row][col];
                if (fscanf(input, "%4u %4u %4u %7u %4u", &c->red, &c->green, &c->blue,
                           &c->codepoint, &c->width) != 5 || c->red > 255 ||
                    c->green > 255 || c->blue > 255)
                    return false;
                if (continuation ? (c->codepoint != 0 || c->width != 0)
                                 : (c->codepoint < 32 || c->codepoint > 0x10ffff ||
                                    (c->codepoint >= 0xd800 && c->codepoint <= 0xdfff) ||
                                    c->width < 1 || c->width > 2 || c->width > f->cols - col))
                    return false;
                continuation = c->width == 2;
            }
        }
    }
    char trailing;
    return fscanf(input, " %c", &trailing) == EOF && !ferror(input);
}
#endif
