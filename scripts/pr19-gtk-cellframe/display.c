/* GTK4 only, fixed synthetic frames. No Forge app, VTE, input or PTY. */
#include <gtk/gtk.h>
#include <pango/pangocairo.h>
#include "frame_wire.h"

enum { CELL_W = 34, CELL_H = 26, CANVAS_W = 160, CANVAS_H = 64 };
typedef struct {
    PaintFrame frames[FRAME_COUNT];
    unsigned phase, draw_count;
    gboolean success;
    GtkWindow *window;
    GtkWidget *canvas;
    GdkPaintable *paintable;
    GMainLoop *loop;
} Fixture;

static void rectangle(cairo_t *cr, int x, int y, int w, int h,
                      unsigned r, unsigned g, unsigned b)
{
    cairo_set_source_rgb(cr, r / 255.0, g / 255.0, b / 255.0);
    cairo_rectangle(cr, x, y, w, h);
    cairo_fill(cr);
}

static void draw(GtkDrawingArea *area, cairo_t *cr, int width, int height, gpointer opaque)
{
    (void)area;
    Fixture *f = opaque;
    const PaintFrame *p = &f->frames[f->phase];
    ++f->draw_count;
    rectangle(cr, 0, 0, width, height, 4, 6, 8);
    for (unsigned r = 0; r < p->rows; ++r)
        for (unsigned c = 0; c < p->cols; ++c) {
            const PaintCell *v = &p->cells[r][c];
            rectangle(cr, c * CELL_W, r * CELL_H, CELL_W, CELL_H,
                      v->red, v->green, v->blue);
        }
    PangoLayout *layout = pango_cairo_create_layout(cr);
    PangoFontDescription *font = pango_font_description_from_string("Noto Sans Mono CJK SC 12");
    pango_layout_set_font_description(layout, font);
    pango_font_description_free(font);
    for (unsigned r = 0; r < p->rows; ++r)
        for (unsigned c = 0; c < p->cols; ++c) {
            const PaintCell *v = &p->cells[r][c];
            if (v->width == 0)
                continue;
            char text[7] = {0};
            pango_layout_set_text(layout, text, g_unichar_to_utf8(v->codepoint, text));
            cairo_save(cr);
            cairo_rectangle(cr, c * CELL_W, r * CELL_H + 4, v->width * CELL_W, CELL_H - 8);
            cairo_clip(cr);
            cairo_set_source_rgb(cr, 1, 1, 1);
            cairo_move_to(cr, c * CELL_W + 4, r * CELL_H + 4);
            pango_cairo_show_layout(cr, layout);
            cairo_restore(cr);
        }
    g_object_unref(layout);
    rectangle(cr, p->cursor_col * CELL_W, p->cursor_row * CELL_H + CELL_H - 3,
              CELL_W, 3, 246, 246, 246);
}

static guint32 pixel(const guint8 *buffer, gsize stride, unsigned x, unsigned y)
{
    guint32 argb;
    memcpy(&argb, buffer + y * stride + x * sizeof(argb), sizeof(argb));
    return argb; /* gdk_texture_download uses CAIRO_FORMAT_ARGB32 */
}

static gboolean glyph_present(const guint8 *buffer, gsize stride, unsigned cell)
{
    for (unsigned y = 4; y < CELL_H - 4; ++y)
        for (unsigned x = cell * CELL_W + 4; x < (cell + 1) * CELL_W; ++x) {
            guint32 color = pixel(buffer, stride, x, y);
            if (((color >> 16) & 255) >= 240 && ((color >> 8) & 255) >= 240 &&
                (color & 255) >= 240)
                return TRUE; /* none of the backgrounds reaches this near-white threshold */
        }
    return FALSE;
}

static gboolean sample(gpointer opaque)
{
    static const guint32 expected[FRAME_COUNT][4] = {
        {0xffd01924, 0xff0fd22a, 0xff0b2cda, 0xffd6b20b},
        {0xff8717c9, 0xff0fd22a, 0xff0b2cda, 0xff040608},
    };
    Fixture *f = opaque;
    if (!gtk_widget_get_mapped(f->canvas) || f->draw_count == 0 ||
        gtk_widget_get_width(f->canvas) != CANVAS_W || gtk_widget_get_height(f->canvas) != CANVAS_H)
        return G_SOURCE_CONTINUE;
    GskRenderer *renderer = gtk_native_get_renderer(GTK_NATIVE(f->window));
    if (renderer == NULL)
        return G_SOURCE_CONTINUE;

    GtkSnapshot *snapshot = gtk_snapshot_new();
    gdk_paintable_snapshot(f->paintable, GDK_SNAPSHOT(snapshot), CANVAS_W, CANVAS_H);
    GskRenderNode *node = gtk_snapshot_free_to_node(snapshot);
    if (node == NULL)
        return G_SOURCE_CONTINUE;
    graphene_rect_t area = GRAPHENE_RECT_INIT(0, 0, CANVAS_W, CANVAS_H);
    GdkTexture *texture = gsk_renderer_render_texture(renderer, node, &area);
    gsk_render_node_unref(node);
    if (texture == NULL)
        return G_SOURCE_CONTINUE;
    if (gdk_texture_get_width(texture) != CANVAS_W || gdk_texture_get_height(texture) != CANVAS_H) {
        g_object_unref(texture);
        return G_SOURCE_CONTINUE;
    }
    guint8 pixels[CANVAS_H * CANVAS_W * 4];
    gsize stride = CANVAS_W * 4;
    gdk_texture_download(texture, pixels, stride);
    g_object_unref(texture);
    unsigned cursor = f->phase == 0 ? 3 : 2;
    for (unsigned c = 0; c < 4; ++c)
        if (pixel(pixels, stride, c * CELL_W + 1, 1) != expected[f->phase][c])
            return G_SOURCE_CONTINUE;
    if (pixel(pixels, stride, cursor * CELL_W + 1, CELL_H - 2) != 0xfff6f6f6 ||
        !glyph_present(pixels, stride, 0) || !glyph_present(pixels, stride, 1) ||
        (f->phase == 0 && !glyph_present(pixels, stride, 3)))
        return G_SOURCE_CONTINUE;
    printf("PR19 mapped pixels revision=%u cell_backgrounds=4 cursor=ok glyph=ok\n", f->phase + 1);
    fflush(stdout);
    if (f->phase + 1 == FRAME_COUNT) {
        f->success = TRUE;
        g_main_loop_quit(f->loop);
        return G_SOURCE_REMOVE;
    }
    ++f->phase;
    f->draw_count = 0; /* require a new actual mapped draw, not just a snapshot */
    gtk_widget_queue_draw(f->canvas);
    return G_SOURCE_CONTINUE;
}

static gboolean expired(gpointer opaque)
{
    Fixture *f = opaque;
    g_main_loop_quit(f->loop);
    return G_SOURCE_CONTINUE;
}

int main(void)
{
    Fixture f = {0};
    if (!read_frames(stdin, f.frames)) {
        fputs("PR19 invalid private fixture frames\n", stderr);
        return 1;
    }
    gtk_init();
    f.window = GTK_WINDOW(gtk_window_new());
    gtk_window_set_decorated(f.window, FALSE);
    gtk_window_set_resizable(f.window, FALSE);
    gtk_window_set_default_size(f.window, CANVAS_W, CANVAS_H);
    f.canvas = gtk_drawing_area_new();
    gtk_widget_set_size_request(f.canvas, CANVAS_W, CANVAS_H);
    gtk_drawing_area_set_draw_func(GTK_DRAWING_AREA(f.canvas), draw, &f, NULL);
    gtk_window_set_child(f.window, f.canvas);
    f.paintable = gtk_widget_paintable_new(f.canvas);
    f.loop = g_main_loop_new(NULL, FALSE);
    guint poll = g_timeout_add(25, sample, &f);
    guint timeout = g_timeout_add(5000, expired, &f);
    gtk_window_present(f.window);
    g_main_loop_run(f.loop);
    if (!f.success)
        g_source_remove(poll);
    g_source_remove(timeout);
    g_object_unref(f.paintable);
    gtk_window_destroy(f.window);
    g_main_loop_unref(f.loop);
    if (!f.success) {
        fputs("PR19 mapped snapshot did not match the fixed frame pixels in time\n", stderr);
        return 1;
    }
    return 0;
}
