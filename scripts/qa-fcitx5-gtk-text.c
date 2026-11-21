/* Dedicated synthetic GTK editing widget. No shell or terminal code. */
#include <gtk/gtk.h>
#include <gdk/x11/gdkx.h>
#include <stdio.h>
#include <string.h>

static void focus_changed(GtkWidget *widget, GParamSpec *unused, gpointer data)
{
    (void)unused;
    (void)data;
    puts(gtk_widget_has_focus(widget) ? "FOCUS ON" : "FOCUS OFF");
}

static void preedit_changed(GtkText *text, const char *value, gpointer data)
{
    (void)text;
    (void)data;
    puts(value != NULL && *value != '\0' ? "PREEDIT ACTIVE" : "PREEDIT CLEAR");
}

static void text_changed(GtkEditable *editable, gpointer data)
{
    (void)data;
    const char *value = gtk_editable_get_text(editable);
    /* Compare UTF-8 bytes of 你好 and never print arbitrary input. */
    if (strcmp(value, "\xe4\xbd\xa0\xe5\xa5\xbd") == 0)
        puts("TEXT EXACT");
    else
        puts(*value == '\0' ? "TEXT EMPTY" : "TEXT OTHER");
}

static void window_realized(GtkWidget *widget, gpointer data)
{
    (void)data;
    GdkSurface *surface = gtk_native_get_surface(GTK_NATIVE(widget));
    if (surface != NULL && GDK_IS_X11_SURFACE(surface))
        printf("WINDOW REALIZED %lu\n", (unsigned long)gdk_x11_surface_get_xid(surface));
    else
        puts("WINDOW REALIZED NO-XID");
}

static void window_mapped(GtkWidget *widget, gpointer data)
{
    (void)widget;
    (void)data;
    puts("WINDOW MAPPED");
}

static void window_unmapped(GtkWidget *widget, gpointer data)
{
    (void)widget;
    (void)data;
    puts("WINDOW UNMAPPED");
}

int main(int argc, char **argv)
{
    const gboolean diagnose = argc == 2 && strcmp(argv[1], "--window-witness") == 0;
    if (argc != 1 && !diagnose)
        return 2;
    setvbuf(stdout, NULL, _IOLBF, 0);
    g_set_prgname("forge-fcitx5-gtk-text-fixture");
    gtk_init();
    if (diagnose)
        puts("WINDOW GTK-READY");
    GtkWidget *window = gtk_window_new();
    if (diagnose) {
        g_signal_connect_after(window, "realize", G_CALLBACK(window_realized), NULL);
        g_signal_connect_after(window, "map", G_CALLBACK(window_mapped), NULL);
        g_signal_connect_after(window, "unmap", G_CALLBACK(window_unmapped), NULL);
    }
    GtkWidget *text = gtk_text_new();
    gtk_window_set_title(GTK_WINDOW(window), "Forge Fcitx5 Pinyin QA");
    gtk_window_set_default_size(GTK_WINDOW(window), 400, 100);
    gtk_window_set_child(GTK_WINDOW(window), text);
    g_signal_connect(text, "notify::has-focus", G_CALLBACK(focus_changed), NULL);
    g_signal_connect(text, "preedit-changed", G_CALLBACK(preedit_changed), NULL);
    g_signal_connect(text, "changed", G_CALLBACK(text_changed), NULL);
    text_changed(GTK_EDITABLE(text), NULL);
    gtk_window_set_focus(GTK_WINDOW(window), text);
    gtk_window_present(GTK_WINDOW(window));
    if (diagnose)
        puts("WINDOW PRESENT-RETURNED");
    GMainLoop *loop = g_main_loop_new(NULL, FALSE);
    g_main_loop_run(loop);
    g_main_loop_unref(loop);
    return 0;
}
