/* Dedicated synthetic GTK editing widget. No shell or terminal code. */
#include <gtk/gtk.h>
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

int main(void)
{
    setvbuf(stdout, NULL, _IOLBF, 0);
    g_set_prgname("forge-fcitx5-gtk-text-fixture");
    gtk_init();
    GtkWidget *window = gtk_window_new();
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
    GMainLoop *loop = g_main_loop_new(NULL, FALSE);
    g_main_loop_run(loop);
    g_main_loop_unref(loop);
    return 0;
}
