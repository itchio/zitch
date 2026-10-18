/* Host build of the shim's GLSL rewrite: prints a desktop shader as the
   ES 3.00 the shim would hand the driver, for `make shim-check`. */
#include "sdl-dynapi.c"

int main(int argc, char **argv)
{
    if (argc != 2) {
        fprintf(stderr, "usage: %s shader\n", argv[0]);
        return 2;
    }
    FILE *f = fopen(argv[1], "rb");
    if (!f) {
        perror(argv[1]);
        return 2;
    }
    char *source = NULL;
    size_t size = 0;
    char buf[4096];
    for (size_t n; (n = fread(buf, 1, sizeof buf, f)) > 0;) {
        char *grown = realloc(source, size + n + 1);
        if (!grown) {
            perror("realloc");
            return 2;
        }
        source = grown;
        memcpy(source + size, buf, n);
        size += n;
    }
    if (ferror(f) || !source) {
        perror(argv[1]);
        return 2;
    }
    fclose(f);
    source[size] = '\0';
    const char *version_line;
    if (!desktop_glsl_version(source, &version_line)) {
        fprintf(stderr, "%s: not a desktop GLSL shader\n", argv[1]);
        return 1;
    }
    char *out = to_es300(version_line);
    fputs(out, stdout);
    free(out);
    free(source);
    return 0;
}
