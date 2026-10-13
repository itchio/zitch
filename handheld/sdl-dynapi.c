/* Routes a game's own SDL2 into the firmware's libSDL2.
 *
 * Linux builds of games usually carry their own SDL2, in the executable
 * or in a library of their own, built on a desktop
 * with only X11, Wayland and KMSDRM display backends. muOS has none of
 * those; the only way onto its screen is the firmware's libSDL2 with its
 * "mali" backend. SDL2 keeps every public call behind a jump table and
 * lets the SDL_DYNAMIC_API variable name a library to fill that table
 * from, which is how zitch hands this one to the games it launches.
 *
 * The firmware library's own table is laid out differently from
 * upstream, so entries are matched by name. Each SDL_* stub in the game
 * loads its pointer from one slot of the table, and reading the stub's
 * code says which; that is checked against the upstream order, which
 * also names the slots that have no stub (the varargs functions). A
 * stripped binary is trusted to follow the upstream order.
 *
 * A game with no SDL2 of its own calls the firmware's, which asks to be
 * overridden too; that one keeps its own entries, and only its GL
 * loader goes through here, for the fixes below.
 */
#define _GNU_SOURCE
#include <ctype.h>
#include <dlfcn.h>
#include <elf.h>
#include <signal.h>
#include <fcntl.h>
#include <link.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <ucontext.h>
#include <unistd.h>

#include "sdl-dynapi-procs.h"

/* By full path: a game may put its own libSDL2 on LD_LIBRARY_PATH, and
   that one cannot open the screen either. ZITCH_SDL_LIB names another. */
static const char *const FIRMWARE_SDL[] = {
    "/usr/lib/libSDL2-2.0.so.0",
    "/usr/lib/aarch64-linux-gnu/libSDL2-2.0.so.0",
};

/* Local, so a library with SDL2 built in and exported keeps calling its
   own entry points, which go through the table, rather than binding to
   the firmware's. */
static void *open_firmware_sdl(const char **path)
{
    *path = getenv("ZITCH_SDL_LIB");
    if (*path) {
        return dlopen(*path, RTLD_NOW | RTLD_LOCAL);
    }
    for (size_t i = 0; i < sizeof(FIRMWARE_SDL) / sizeof(FIRMWARE_SDL[0]); i++) {
        void *lib = dlopen(FIRMWARE_SDL[i], RTLD_NOW | RTLD_LOCAL);
        if (lib) {
            *path = FIRMWARE_SDL[i];
            return lib;
        }
    }
    return NULL;
}
#define UPSTREAM_COUNT (sizeof(UPSTREAM) / sizeof(UPSTREAM[0]))

/* The loaded object holding the SDL2 that asked: the executable, or a
   library the game loaded with SDL2 built in. */
struct sdl_image {
    uintptr_t table;
    uintptr_t bias;
    /* Empty for the executable. */
    const char *path;
};

static int find_image(struct dl_phdr_info *info, size_t size, void *data)
{
    (void)size;
    struct sdl_image *image = data;
    for (int i = 0; i < info->dlpi_phnum; i++) {
        const ElfW(Phdr) *ph = &info->dlpi_phdr[i];
        uintptr_t start = info->dlpi_addr + ph->p_vaddr;
        if (ph->p_type == PT_LOAD && image->table >= start
            && image->table < start + ph->p_memsz) {
            image->bias = info->dlpi_addr;
            image->path = info->dlpi_name ? info->dlpi_name : "";
            return 1;
        }
    }
    return 0;
}

static int same_file(const char *a, const char *b)
{
    struct stat sa, sb;
    return stat(a, &sa) == 0 && stat(b, &sb) == 0 && sa.st_dev == sb.st_dev
        && sa.st_ino == sb.st_ino;
}

/* The firmware library asks too, on its first call. */
static int is_firmware_sdl(const char *path)
{
    const char *custom = getenv("ZITCH_SDL_LIB");
    if (custom) {
        return same_file(path, custom);
    }
    for (size_t i = 0; i < sizeof(FIRMWARE_SDL) / sizeof(FIRMWARE_SDL[0]); i++) {
        if (same_file(path, FIRMWARE_SDL[i])) {
            return 1;
        }
    }
    return 0;
}

/* A stub is `adrp xN, page; ldr xN, [xN, #off]; ...; br x16`, sometimes
   with a few register moves in between, and in an unoptimized build an
   `add xN, xN, #lo` between the adrp and the ldr. Returns the address
   the ldr reads, the table slot, or 0 for code of another shape. */
static uintptr_t stub_slot(uintptr_t pc)
{
    const uint32_t *code = (const uint32_t *)pc;
    uintptr_t page = 0;
    unsigned reg = 32;
    for (int i = 0; i < 8; i++) {
        uint32_t insn = code[i];
        if ((insn & 0x9f000000) == 0x90000000) {
            int64_t imm = (int64_t)((((insn >> 5) & 0x7ffff) << 2) | ((insn >> 29) & 3));
            imm = (imm << 43) >> 43;
            page = ((pc + 4 * i) & ~(uintptr_t)0xfff) + (uintptr_t)(imm << 12);
            reg = insn & 31;
        } else if ((insn & 0xff800000) == 0x91000000 && ((insn >> 5) & 31) == reg) {
            uintptr_t imm = (insn >> 10) & 0xfff;
            page += (insn & (1u << 22)) ? imm << 12 : imm;
            reg = insn & 31;
        } else if ((insn & 0xffc00000) == 0xf9400000 && ((insn >> 5) & 31) == reg) {
            return page + ((insn >> 10) & 0xfff) * 8;
        } else if (insn == 0xd61f0200 || (insn & 0x7c000000) == 0x14000000) {
            return 0;
        }
    }
    return 0;
}

/* Fills `names` with the slot each SDL_* stub in the image reads
   from, as copies the caller frees. Returns the number found, or -1
   when a stub contradicts the upstream order. */
static int read_stubs(const struct sdl_image *image, uint32_t count, const char **names)
{
    uintptr_t table = image->table;
    int fd = open(image->path[0] ? image->path : "/proc/self/exe", O_RDONLY | O_CLOEXEC);
    if (fd < 0) {
        return 0;
    }
    struct stat st;
    if (fstat(fd, &st) < 0) {
        close(fd);
        return 0;
    }
    const uint8_t *map = mmap(NULL, st.st_size, PROT_READ, MAP_PRIVATE, fd, 0);
    close(fd);
    if (map == MAP_FAILED) {
        return 0;
    }
    int found = 0;
    const Elf64_Ehdr *eh = (const Elf64_Ehdr *)map;
    if (memcmp(eh->e_ident, ELFMAG, SELFMAG) != 0 || eh->e_ident[EI_CLASS] != ELFCLASS64) {
        goto done;
    }
    const Elf64_Shdr *sh = (const Elf64_Shdr *)(map + eh->e_shoff);
    for (int s = 0; s < eh->e_shnum && found >= 0; s++) {
        if (sh[s].sh_type != SHT_SYMTAB && sh[s].sh_type != SHT_DYNSYM) {
            continue;
        }
        const Elf64_Sym *sym = (const Elf64_Sym *)(map + sh[s].sh_offset);
        size_t nsym = sh[s].sh_size / sizeof(Elf64_Sym);
        const char *str = (const char *)(map + sh[sh[s].sh_link].sh_offset);
        for (size_t i = 0; i < nsym; i++) {
            const char *name = str + sym[i].st_name;
            if (ELF64_ST_TYPE(sym[i].st_info) != STT_FUNC || sym[i].st_value == 0
                || strncmp(name, "SDL_", 4) != 0) {
                continue;
            }
            size_t len = strlen(name);
            if (len > 5 && strcmp(name + len - 5, "_REAL") == 0) {
                continue;
            }
            uintptr_t slot = stub_slot(image->bias + sym[i].st_value);
            if (slot < table || (slot - table) / sizeof(void *) >= count) {
                continue;
            }
            size_t index = (slot - table) / sizeof(void *);
            if (index >= UPSTREAM_COUNT || strcmp(UPSTREAM[index], name) != 0) {
                fprintf(stderr, "sdl-dynapi: %s sits at slot %zu, expected %s\n", name, index,
                        index < UPSTREAM_COUNT ? UPSTREAM[index] : "nothing");
                found = -1;
                break;
            }
            if (!names[index]) {
                names[index] = strdup(name);
                found++;
            }
        }
    }
done:
    munmap((void *)map, st.st_size);
    return found;
}

/* Left in slots past the end of the known order, where the caller's SDL
   is newer than this list. Anything is better than the caller's default
   entry, which re-enters the override and recurses until the stack ends. */
static void unknown_entry(void)
{
    fprintf(stderr, "sdl-dynapi: the game called an SDL function newer than this shim knows\n");
    abort();
}

/* A game on a desktop shader translator (FNA's MojoShader) hands the
   driver GLSL ES 1.00, which has a single color target. A shader that
   writes to more is rewritten as GLSL ES 3.00, and so is every shader
   it is linked with, as one program cannot mix the two versions. */

#define GL_EXTENSIONS_ 0x1F03
#define GL_VERSION_ 0x1F02
#define GL_SHADER_TYPE_ 0x8B4F
#define GL_FRAGMENT_SHADER_ 0x8B30

static void *(*real_get_proc)(const char *name);
static int tracing(void);
static void (*real_shader_source)(unsigned shader, int count, const char *const *strings,
                                  const int *lengths);
static void (*real_link_program)(unsigned program);
static void (*real_delete_shader)(unsigned shader);

/* The GLSL ES 1.00 shaders the game has, as it wrote them. */
struct shader {
    unsigned id;
    char *source;
    int rewritten;
    struct shader *next;
};
static struct shader *shaders;

static struct shader *find_shader(unsigned id)
{
    for (struct shader *s = shaders; s; s = s->next) {
        if (s->id == id) {
            return s;
        }
    }
    return NULL;
}

static void forget_shader(unsigned id)
{
    for (struct shader **at = &shaders; *at; at = &(*at)->next) {
        if ((*at)->id == id) {
            struct shader *gone = *at;
            *at = gone->next;
            free(gone->source);
            free(gone);
            return;
        }
    }
}

/* Whether the context takes GLSL ES 3.00. */
static int has_es3(void)
{
    static int known = -1;
    if (known < 0) {
        const char *(*get_string)(unsigned) = real_get_proc("glGetString");
        const char *version = get_string ? get_string(GL_VERSION_) : NULL;
        known = version && strncmp(version, "OpenGL ES ", 10) == 0 && version[10] >= '3';
    }
    return known;
}

/* Whether the shader writes to a color target past the first. */
static int writes_more_targets(const char *source)
{
    static const char FRAG_DATA[] = "gl_FragData[";
    for (const char *at = source; (at = strstr(at, FRAG_DATA));) {
        at += sizeof(FRAG_DATA) - 1;
        if (*at >= '1' && *at <= '9') {
            return 1;
        }
    }
    return 0;
}

static int is_word(char c)
{
    return (c >= 'a' && c <= 'z') || (c >= 'A' && c <= 'Z') || (c >= '0' && c <= '9') || c == '_';
}

/* GLSL ES 1.00 names and what GLSL ES 3.00 calls them. */
static const char *const RENAMED[][2] = {
    {"texture2D", "texture"},
    {"texture2DProj", "textureProj"},
    {"texture2DLod", "textureLod"},
    {"texture2DLodEXT", "textureLod"},
    {"texture2DProjLod", "textureProjLod"},
    {"texture2DProjLodEXT", "textureProjLod"},
    {"texture2DGradEXT", "textureGrad"},
    {"texture3D", "texture"},
    {"textureCube", "texture"},
    {"textureCubeLod", "textureLod"},
    {"textureCubeLodEXT", "textureLod"},
    {"textureCubeGradEXT", "textureGrad"},
    {"gl_FragDepthEXT", "gl_FragDepth"},
    {"gl_FragColor", "zitch_FragData0"},
};

#define MAX_TARGETS 8
/* Room for the longest replacement of one word. */
#define WORD_ROOM 32
#define TARGET_DECL "layout(location = %d) out highp vec4 zitch_FragData%d;\n"

/* The GLSL ES 1.00 `source` as GLSL ES 3.00, for the caller to free. */
static char *to_es3(const char *source, int fragment)
{
    const char *body = strchr(source, '\n');
    if (!body) {
        return NULL;
    }
    body++;
    /* No replacement is longer than its word plus WORD_ROOM, and a
       word is at least two bytes with its separator. */
    size_t room = 64 + MAX_TARGETS * (sizeof(TARGET_DECL) + 8) + strlen(body) * (WORD_ROOM / 2 + 1);
    char *out = malloc(room);
    char *text = malloc(strlen(body) * (WORD_ROOM / 2 + 1) + 1);
    if (!out || !text) {
        free(out);
        free(text);
        return NULL;
    }
    unsigned targets = 0;
    char *to = text;
    const char *at = body;
    while (*at) {
        if (*at == '#' && strncmp(at, "#extension", 10) == 0) {
            /* What 1.00 needed extensions for is built into 3.00. */
            while (*at && *at != '\n') {
                at++;
            }
            continue;
        }
        if (!is_word(*at) || (at > body && is_word(at[-1]))) {
            *to++ = *at++;
            continue;
        }
        size_t len = 1;
        while (is_word(at[len])) {
            len++;
        }
        const char *word = NULL;
        if (len == 9 && memcmp(at, "attribute", 9) == 0) {
            word = "in";
        } else if (len == 7 && memcmp(at, "varying", 7) == 0) {
            word = fragment ? "in" : "out";
        } else if (len == 11 && memcmp(at, "gl_FragData", 11) == 0 && at[11] == '['
                   && at[12] >= '0' && at[12] < '0' + MAX_TARGETS && at[13] == ']') {
            targets |= 1u << (at[12] - '0');
            to += sprintf(to, "zitch_FragData%c", at[12]);
            at += 14;
            continue;
        } else {
            for (size_t i = 0; i < sizeof(RENAMED) / sizeof(RENAMED[0]); i++) {
                if (strlen(RENAMED[i][0]) == len && memcmp(at, RENAMED[i][0], len) == 0) {
                    word = RENAMED[i][1];
                    if (strcmp(word, "zitch_FragData0") == 0) {
                        targets |= 1;
                    }
                    break;
                }
            }
        }
        if (word) {
            to += sprintf(to, "%s", word);
        } else {
            memcpy(to, at, len);
            to += len;
        }
        at += len;
    }
    *to = '\0';
    char *head = out + sprintf(out, "#version 300 es\n");
    for (int i = 0; fragment && i < MAX_TARGETS; i++) {
        if (targets & (1u << i)) {
            head += sprintf(head, TARGET_DECL, i, i);
        }
    }
    strcpy(head, text);
    free(text);
    return out;
}

/* Hands the driver the 3.00 form of a shader the game wrote as 1.00. */
static int rewrite(struct shader *shader)
{
    void (*get_shader)(unsigned, unsigned, int *) = real_get_proc("glGetShaderiv");
    int type = 0;
    if (!get_shader) {
        return 0;
    }
    get_shader(shader->id, GL_SHADER_TYPE_, &type);
    char *modern = to_es3(shader->source, type == GL_FRAGMENT_SHADER_);
    if (!modern) {
        return 0;
    }
    const char *one = modern;
    real_shader_source(shader->id, 1, &one, NULL);
    free(modern);
    shader->rewritten = 1;
    return 1;
}

/* GLSL ES 3.00 has no implicit int to float conversion; desktop GLSL
   does. Rewrites the integer literals desktop code relies on being
   promoted: arguments to the source's own float-typed parameters,
   operands beside a float literal, and float declaration initializers.
   Constructors convert on their own and are left alone. */

struct glsl_function {
    const char *name;
    size_t len;
    int params;
    unsigned char is_float[16];
};

static int word_is(const char *s, size_t len, const char *word)
{
    return strlen(word) == len && memcmp(s, word, len) == 0;
}

static int glsl_float_type(const char *s, size_t len)
{
    return word_is(s, len, "float") || word_is(s, len, "vec2") || word_is(s, len, "vec3")
        || word_is(s, len, "vec4") || word_is(s, len, "mat2") || word_is(s, len, "mat3")
        || word_is(s, len, "mat4");
}

static int glsl_return_type(const char *s, size_t len)
{
    return glsl_float_type(s, len) || word_is(s, len, "void") || word_is(s, len, "int")
        || word_is(s, len, "bool");
}

static int glsl_qualifier(const char *s, size_t len)
{
    return word_is(s, len, "in") || word_is(s, len, "out") || word_is(s, len, "inout")
        || word_is(s, len, "const");
}

static int glsl_ident(char c)
{
    return isalnum((unsigned char)c) || c == '_';
}

static const char *skip_blanks(const char *p)
{
    while (*p == ' ' || *p == '\t') {
        p++;
    }
    return p;
}

static const char *read_word(const char *p, const char **word, size_t *len)
{
    *word = p;
    while (glsl_ident(*p)) {
        p++;
    }
    *len = (size_t)(p - *word);
    return p;
}

/* The functions the source defines, by their line-initial prototypes. */
static int glsl_functions(const char *src, struct glsl_function *fns, int max)
{
    int count = 0;
    for (const char *line = src; line && *line && count < max; line = strchr(line, '\n'),
                    line = line ? line + 1 : NULL) {
        const char *type, *name;
        size_t tlen, nlen;
        const char *p = read_word(skip_blanks(line), &type, &tlen);
        if (!tlen || *p != ' ') {
            continue;
        }
        p = skip_blanks(read_word(p + 1, &name, &nlen));
        if (!nlen || *p != '(' || !glsl_return_type(type, tlen)) {
            continue;
        }
        struct glsl_function *fn = &fns[count];
        fn->name = name;
        fn->len = nlen;
        fn->params = 0;
        p++;
        while (*p && *p != ')' && fn->params < 16) {
            const char *ptype;
            size_t plen;
            while (*p == ' ' || *p == '\n') {
                p++;
            }
            p = read_word(p, &ptype, &plen);
            if (glsl_qualifier(ptype, plen)) {
                p = read_word(skip_blanks(p), &ptype, &plen);
            }
            fn->is_float[fn->params++] = glsl_float_type(ptype, plen);
            while (*p && *p != ',' && *p != ')') {
                p++;
            }
            if (*p == ',') {
                p++;
            }
        }
        count++;
    }
    return count;
}

/* Whether the literal before `at` (ending just before it, blanks
   skipped) or after it is a float one. */
static int float_literal_before(const char *src, const char *at)
{
    while (at > src && at[-1] == ' ') {
        at--;
    }
    const char *start = at;
    while (start > src && (isalnum((unsigned char)start[-1]) || start[-1] == '.')) {
        start--;
    }
    return isdigit((unsigned char)*start) && memchr(start, '.', (size_t)(at - start)) != NULL;
}

static int float_literal_after(const char *at)
{
    at = skip_blanks(at);
    const char *end = at;
    while (isalnum((unsigned char)*end) || *end == '.') {
        end++;
    }
    return isdigit((unsigned char)*at) && memchr(at, '.', (size_t)(end - at)) != NULL;
}

/* Whether `before` ends a `float x =` style declaration head. */
static int float_declaration_before(const char *src, const char *before)
{
    const char *p = before - 1;
    while (p > src && p[-1] == ' ') {
        p--;
    }
    while (p > src && glsl_ident(p[-1])) {
        p--;
    }
    while (p > src && p[-1] == ' ') {
        p--;
    }
    const char *type = p;
    while (type > src && glsl_ident(type[-1])) {
        type--;
    }
    return glsl_float_type(type, (size_t)(p - type));
}

static char *glsl_promote_ints(const char *src)
{
    struct glsl_function fns[64];
    int nfns = glsl_functions(src, fns, 64);
    char *out = malloc(strlen(src) * 2 + 16);
    if (!out) {
        return NULL;
    }
    struct {
        int fn;
        int arg;
        int depth;
    } calls[32];
    int ncalls = 0, depth = 0, bracket = 0, layout_depth = -1;
    size_t o = 0;
    const char *p = src;
    while (*p) {
        if (*p == '#' || (*p == '/' && p[1] == '/')) {
            while (*p && *p != '\n') {
                out[o++] = *p++;
            }
            continue;
        }
        if (*p == '/' && p[1] == '*') {
            while (*p && !(*p == '*' && p[1] == '/')) {
                out[o++] = *p++;
            }
            if (*p) {
                out[o++] = *p++;
                out[o++] = *p++;
            }
            continue;
        }
        if (glsl_ident(*p) && !isdigit((unsigned char)*p)) {
            const char *id;
            size_t len;
            p = read_word(p, &id, &len);
            memcpy(out + o, id, len);
            o += len;
            if (*skip_blanks(p) != '(') {
                continue;
            }
            if (word_is(id, len, "layout")) {
                layout_depth = depth + 1;
            }
            for (int i = 0; i < nfns && ncalls < 32; i++) {
                if (fns[i].len == len && memcmp(fns[i].name, id, len) == 0) {
                    calls[ncalls].fn = i;
                    calls[ncalls].arg = 0;
                    calls[ncalls].depth = depth + 1;
                    ncalls++;
                    break;
                }
            }
            continue;
        }
        if (isdigit((unsigned char)*p) || (*p == '.' && isdigit((unsigned char)p[1]))) {
            const char *num = p;
            int is_float = 0;
            while (isalnum((unsigned char)*p) || *p == '.') {
                if (strchr(".eEfFxXuU", *p)) {
                    is_float = 1;
                }
                p++;
            }
            memcpy(out + o, num, (size_t)(p - num));
            o += (size_t)(p - num);
            if (is_float || bracket || (layout_depth >= 0 && depth >= layout_depth)) {
                continue;
            }
            const char *before = num;
            while (before > src && (before[-1] == ' ' || before[-1] == '\t')) {
                before--;
            }
            const char *after = skip_blanks(p);
            int promote = 0;
            int whole_arg = before > src && (before[-1] == '(' || before[-1] == ',')
                && (*after == ',' || *after == ')');
            if (ncalls && calls[ncalls - 1].depth == depth && whole_arg) {
                struct glsl_function *fn = &fns[calls[ncalls - 1].fn];
                int arg = calls[ncalls - 1].arg;
                promote = arg < fn->params && fn->is_float[arg];
            }
            if (!promote && before > src && strchr("*/+-", before[-1])) {
                promote = float_literal_before(src, before - 1);
            }
            if (!promote && strchr("*/+-", *after) && after[1] != '=') {
                promote = float_literal_after(after + 1);
            }
            if (!promote && before > src && before[-1] == '=' && (*after == ';' || *after == ',')) {
                promote = float_declaration_before(src, before);
            }
            if (promote) {
                out[o++] = '.';
                out[o++] = '0';
            }
            continue;
        }
        switch (*p) {
        case '(':
            depth++;
            break;
        case ')':
            if (ncalls && calls[ncalls - 1].depth == depth) {
                ncalls--;
            }
            if (layout_depth == depth) {
                layout_depth = -1;
            }
            depth--;
            break;
        case '[':
            bracket++;
            break;
        case ']':
            if (bracket) {
                bracket--;
            }
            break;
        case ',':
            if (ncalls && calls[ncalls - 1].depth == depth) {
                calls[ncalls - 1].arg++;
            }
            break;
        }
        out[o++] = *p++;
    }
    out[o] = '\0';
    return out;
}

/* The GLSL version a desktop shader declares, 130 through 460, or 0. */
static int desktop_glsl_version(const char *source)
{
    while (*source == ' ' || *source == '\t' || *source == '\n' || *source == '\r') {
        source++;
    }
    if (strncmp(source, "#version", 8) != 0) {
        return 0;
    }
    source += 8;
    int version = (int)strtol(source, (char **)&source, 10);
    while (*source == ' ' || *source == '\t') {
        source++;
    }
    return version >= 130 && version <= 460 && strncmp(source, "es", 2) != 0 ? version : 0;
}

/* Desktop GLSL in the in/out style is ES 3.00 but for the version line,
   the precision it has to declare, and the integer literals it leaves
   to be promoted. */
static char *to_es300(const char *source)
{
    const char *rest = strchr(source, '\n');
    rest = rest ? rest + 1 : "";
    static const char head[] = "#version 300 es\n"
                               "precision highp float;\n"
                               "precision highp int;\n"
                               "precision highp sampler2D;\n"
                               "precision highp sampler3D;\n"
                               "precision highp samplerCube;\n"
                               "precision highp sampler2DArray;\n"
                               "precision highp sampler2DShadow;\n";
    char *out = malloc(sizeof(head) + strlen(rest));
    if (!out) {
        return NULL;
    }
    strcpy(out, head);
    strcat(out, rest);
    char *promoted = glsl_promote_ints(out);
    if (promoted) {
        free(out);
        out = promoted;
    }
    return out;
}

static void shader_source(unsigned shader, int count, const char *const *strings,
                          const int *lengths)
{
    forget_shader(shader);
    size_t total = 0;
    for (int i = 0; i < count; i++) {
        total += lengths && lengths[i] >= 0 ? (size_t)lengths[i] : strlen(strings[i]);
    }
    char *source = malloc(total + 1);
    struct shader *entry = calloc(1, sizeof(*entry));
    if (!source || !entry) {
        free(source);
        free(entry);
        real_shader_source(shader, count, strings, lengths);
        return;
    }
    size_t used = 0;
    for (int i = 0; i < count; i++) {
        size_t len = lengths && lengths[i] >= 0 ? (size_t)lengths[i] : strlen(strings[i]);
        memcpy(source + used, strings[i], len);
        used += len;
    }
    source[used] = '\0';
    if (desktop_glsl_version(source) && has_es3()) {
        char *modern = to_es300(source);
        if (modern) {
            if (tracing()) {
                fprintf(stderr, "sdl-dynapi: shader %u rewritten as ES 3.00:\n%s\n", shader,
                        modern);
            }
            const char *one = modern;
            real_shader_source(shader, 1, &one, NULL);
            free(modern);
            free(source);
            free(entry);
            return;
        }
    }
    if (strncmp(source, "#version 100", 12) != 0 || !has_es3()) {
        free(source);
        free(entry);
        real_shader_source(shader, count, strings, lengths);
        return;
    }
    entry->id = shader;
    entry->source = source;
    entry->next = shaders;
    shaders = entry;
    if (!writes_more_targets(source) || !rewrite(entry)) {
        real_shader_source(shader, count, strings, lengths);
    }
}

/* Brings the program's shaders to one version before they link. */
static void link_program(unsigned program)
{
    void (*get_attached)(unsigned, int, int *, unsigned *) = real_get_proc("glGetAttachedShaders");
    void (*compile)(unsigned) = real_get_proc("glCompileShader");
    unsigned ids[8];
    int count = 0;
    if (shaders && get_attached && compile) {
        get_attached(program, 8, &count, ids);
    }
    int rewritten = 0;
    for (int i = 0; i < count; i++) {
        struct shader *shader = find_shader(ids[i]);
        rewritten |= shader && shader->rewritten;
    }
    for (int i = 0; rewritten && i < count; i++) {
        struct shader *shader = find_shader(ids[i]);
        if (shader && !shader->rewritten && rewrite(shader)) {
            compile(shader->id);
        }
    }
    real_link_program(program);
}

static void delete_shader(unsigned shader)
{
    forget_shader(shader);
    real_delete_shader(shader);
}

/* A game that asks for a desktop core profile gets the ES 3.2 context
   the firmware has, with the desktop GL wrappers below and the shader
   rewrite in shader_source. Celeste 64 asks for 3.3 core, forward
   compatible, which the firmware's EGL rejects. The flag follows the
   latest profile asked for: LÖVE asks for 3.2 core, rejects the ES
   context it gets for it, and asks again for 2.1 and then ES, and
   would write GLSL 120 if the version string still read as desktop
   GL then. A compatibility profile is left to fail as before. */
static int desktop_profile;
static int (*real_set_attribute)(int attr, int value);

static int set_attribute(int attr, int value)
{
    enum {
        CONTEXT_MAJOR_VERSION = 17,
        CONTEXT_MINOR_VERSION = 18,
        CONTEXT_FLAGS = 20,
        CONTEXT_PROFILE_MASK = 21,
        CONTEXT_PROFILE_CORE = 1,
        CONTEXT_PROFILE_ES = 4,
        CONTEXT_FORWARD_COMPATIBLE = 2
    };
    switch (attr) {
    case CONTEXT_PROFILE_MASK:
        desktop_profile = value == CONTEXT_PROFILE_CORE;
        if (desktop_profile) {
            value = CONTEXT_PROFILE_ES;
        }
        break;
    case CONTEXT_MAJOR_VERSION:
        if (value > 3) {
            value = 3;
        }
        break;
    case CONTEXT_MINOR_VERSION:
        if (value > 2) {
            value = 2;
        }
        break;
    case CONTEXT_FLAGS:
        value &= ~CONTEXT_FORWARD_COMPATIBLE;
        break;
    }
    if (tracing()) {
        fprintf(stderr, "sdl-dynapi: SDL_GL_SetAttribute(%d, %d)\n", attr, value);
    }
    return real_set_attribute(attr, value);
}

/* MonoGame's DesktopGL build runs on the firmware's SDL2 and so lands
   on this ES context, but reads the version string as desktop GL
   ("3.2 ...") and takes 1.1 from "OpenGL ES 3.2 ...", then gives up
   for want of framebuffer objects. Its shaders are the GLSL ES ones it
   ships for phones, so past that check it only calls a few desktop
   entry points ES lacks. */

static int desktop_gl_game(void)
{
    static int known = -1;
    if (desktop_profile) {
        return 1;
    }
    if (known < 0) {
        char exe[4096];
        ssize_t len = readlink("/proc/self/exe", exe, sizeof(exe) - 1);
        known = 0;
        if (len > 0) {
            exe[len] = '\0';
            char *slash = strrchr(exe, '/');
            if (slash && (size_t)(slash - exe) + sizeof("/MonoGame.Framework.dll") <= sizeof(exe)) {
                strcpy(slash + 1, "MonoGame.Framework.dll");
                known = access(exe, F_OK) == 0;
            }
        }
        if (!known) {
            known = access("MonoGame.Framework.dll", F_OK) == 0;
        }
    }
    return known;
}

static const char *(*real_get_string)(unsigned name);
static void (*real_depth_rangef)(float near, float far);
static void (*real_clear_depthf)(float depth);
static void (*real_draw_buffers)(int count, const unsigned *buffers);

static const char *get_string(unsigned name)
{
    static char version[128];
    const char *real = real_get_string(name);
    if (name != GL_VERSION_ || !real || strncmp(real, "OpenGL ES ", 10) != 0) {
        return real;
    }
    if (!version[0]) {
        snprintf(version, sizeof(version), "%.3s %s", real + 10, real);
    }
    return version;
}

static void polygon_mode(unsigned face, unsigned mode)
{
    (void)face;
    (void)mode;
}

static void depth_range(double near, double far)
{
    real_depth_rangef((float)near, (float)far);
}

static void clear_depth(double depth)
{
    real_clear_depthf((float)depth);
}

static void draw_buffer(unsigned mode)
{
    real_draw_buffers(1, &mode);
}

/* The default framebuffer's buffers are GL_BACK on ES, not the
   left/right pairs desktop code names. */
static void (*real_draw_buffers_n)(int count, const unsigned *buffers);

static void draw_buffers_n(int count, const unsigned *buffers)
{
    static const unsigned back = 0x0405;
    if (count == 1 && buffers[0] >= 0x0400 && buffers[0] <= 0x0408) {
        buffers = &back;
    }
    real_draw_buffers_n(count, buffers);
}

#define GL_READ_FRAMEBUFFER_ 0x8CA8
#define GL_READ_FRAMEBUFFER_BINDING_ 0x8CAA
#define GL_COLOR_ATTACHMENT0_ 0x8CE0
#define GL_TEXTURE_BINDING_2D_ 0x8069
#define GL_TEXTURE_BINDING_CUBE_MAP_ 0x8514
#define GL_TEXTURE_CUBE_MAP_POSITIVE_X_ 0x8515
#define GL_TEXTURE_CUBE_MAP_NEGATIVE_Z_ 0x851A
#define GL_FRAMEBUFFER_COMPLETE_ 0x8CD5
#define GL_TEXTURE_WIDTH_ 0x1000
#define GL_TEXTURE_HEIGHT_ 0x1001
#define GL_BUFFER_SIZE_ 0x8764
#define GL_READ_ONLY_ 0x88B8
#define GL_WRITE_ONLY_ 0x88B9
#define GL_MAP_READ_BIT_ 0x1
#define GL_MAP_WRITE_BIT_ 0x2

static void (*gl_get_integerv)(unsigned, int *);
static void (*gl_get_tex_level_parameteriv)(unsigned, int, unsigned, int *);
static void (*gl_gen_framebuffers)(int, unsigned *);
static void (*gl_delete_framebuffers)(int, const unsigned *);
static void (*gl_bind_framebuffer)(unsigned, unsigned);
static void (*gl_framebuffer_texture2d)(unsigned, unsigned, unsigned, unsigned, int);
static void (*gl_read_pixels)(int, int, int, int, unsigned, unsigned, void *);
static unsigned (*gl_check_framebuffer_status)(unsigned);
static void (*gl_get_buffer_parameteriv)(unsigned, unsigned, int *);
static void *(*gl_map_buffer_range)(unsigned, intptr_t, intptr_t, unsigned);

/* ES reads a texture back through a framebuffer. */
static void get_tex_image(unsigned target, int level, unsigned format, unsigned type, void *pixels)
{
    int texture = 0, previous = 0, width = 0, height = 0;
    unsigned fbo = 0;
    int cube = target >= GL_TEXTURE_CUBE_MAP_POSITIVE_X_ && target <= GL_TEXTURE_CUBE_MAP_NEGATIVE_Z_;
    gl_get_integerv(cube ? GL_TEXTURE_BINDING_CUBE_MAP_ : GL_TEXTURE_BINDING_2D_, &texture);
    gl_get_integerv(GL_READ_FRAMEBUFFER_BINDING_, &previous);
    gl_get_tex_level_parameteriv(target, level, GL_TEXTURE_WIDTH_, &width);
    gl_get_tex_level_parameteriv(target, level, GL_TEXTURE_HEIGHT_, &height);
    gl_gen_framebuffers(1, &fbo);
    gl_bind_framebuffer(GL_READ_FRAMEBUFFER_, fbo);
    gl_framebuffer_texture2d(GL_READ_FRAMEBUFFER_, GL_COLOR_ATTACHMENT0_, target, texture, level);
    if (gl_check_framebuffer_status(GL_READ_FRAMEBUFFER_) == GL_FRAMEBUFFER_COMPLETE_) {
        gl_read_pixels(0, 0, width, height, format, type, pixels);
    } else {
        fprintf(stderr, "sdl-dynapi: glGetTexImage: texture %d target %#x is not readable\n",
                texture, target);
    }
    gl_bind_framebuffer(GL_READ_FRAMEBUFFER_, previous);
    gl_delete_framebuffers(1, &fbo);
}

static void *map_buffer(unsigned target, unsigned access)
{
    int size = 0;
    gl_get_buffer_parameteriv(target, GL_BUFFER_SIZE_, &size);
    unsigned bits = access == GL_READ_ONLY_    ? GL_MAP_READ_BIT_
                    : access == GL_WRITE_ONLY_ ? GL_MAP_WRITE_BIT_
                                               : GL_MAP_READ_BIT_ | GL_MAP_WRITE_BIT_;
    return gl_map_buffer_range(target, 0, size, bits);
}

static int load_gl(void **slot, const char *name)
{
    if (!*slot) {
        *slot = real_get_proc(name);
    }
    return *slot != NULL;
}

static void *desktop_gl_proc(const char *name, void *fn)
{
    if (strcmp(name, "glGetString") == 0) {
        real_get_string = fn;
        return (void *)get_string;
    }
    if (fn && strcmp(name, "glDrawBuffers") == 0) {
        real_draw_buffers_n = fn;
        return (void *)draw_buffers_n;
    }
    if (fn) {
        return fn;
    }
    if (strcmp(name, "glPolygonMode") == 0) {
        return (void *)polygon_mode;
    }
    if (strcmp(name, "glDepthRange") == 0 && (real_depth_rangef = real_get_proc("glDepthRangef"))) {
        return (void *)depth_range;
    }
    if (strcmp(name, "glClearDepth") == 0 && (real_clear_depthf = real_get_proc("glClearDepthf"))) {
        return (void *)clear_depth;
    }
    if (strcmp(name, "glDrawBuffer") == 0 && (real_draw_buffers = real_get_proc("glDrawBuffers"))) {
        return (void *)draw_buffer;
    }
    if (strcmp(name, "glGetTexImage") == 0 && load_gl((void **)&gl_get_integerv, "glGetIntegerv")
        && load_gl((void **)&gl_get_tex_level_parameteriv, "glGetTexLevelParameteriv")
        && load_gl((void **)&gl_gen_framebuffers, "glGenFramebuffers")
        && load_gl((void **)&gl_delete_framebuffers, "glDeleteFramebuffers")
        && load_gl((void **)&gl_bind_framebuffer, "glBindFramebuffer")
        && load_gl((void **)&gl_framebuffer_texture2d, "glFramebufferTexture2D")
        && load_gl((void **)&gl_read_pixels, "glReadPixels")
        && load_gl((void **)&gl_check_framebuffer_status, "glCheckFramebufferStatus")) {
        return (void *)get_tex_image;
    }
    if (strcmp(name, "glMapBuffer") == 0
        && load_gl((void **)&gl_get_buffer_parameteriv, "glGetBufferParameteriv")
        && load_gl((void **)&gl_map_buffer_range, "glMapBufferRange")) {
        return (void *)map_buffer;
    }
    return NULL;
}

/* gl4es, when the game loaded it as its libGL: the firmware's build has
   no constructor and expects the host to hand it the GLES loader and
   start it once a context is current. */
static void *(*real_create_context)(void *window);
static void (*real_get_drawable_size)(void *window, int *w, int *h);
static void *gl_window;

static void main_fb_size(int *w, int *h)
{
    if (gl_window && real_get_drawable_size) {
        real_get_drawable_size(gl_window, w, h);
    }
}

static void *gles_proc(const char *name)
{
    return real_get_proc ? real_get_proc(name) : NULL;
}

static void *create_context(void *window)
{
    void *context = real_create_context(window);
    if (!context) {
        return NULL;
    }
    gl_window = window;
    void (*set_proc)(void *(*)(const char *)) = dlsym(RTLD_DEFAULT, "set_getprocaddress");
    void (*set_fb)(void (*)(int *, int *)) = dlsym(RTLD_DEFAULT, "set_getmainfbsize");
    void (*init)(void) = dlsym(RTLD_DEFAULT, "initialize_gl4es");
    if (set_proc && init) {
        setenv("LIBGL_ES", "2", 0);
        set_proc(gles_proc);
        if (set_fb) {
            set_fb(main_fb_size);
        }
        init();
        fprintf(stderr, "sdl-dynapi: gl4es started on the firmware's context\n");
    }
    return context;
}

/* The firmware's video driver makes no cursors, and there is no mouse
   to show one. A game that insists on one gets a stand-in, which
   SDL_SetCursor and SDL_FreeCursor pass over as not one of theirs.
   LÖVE raises an error when SDL returns none, and Labyrinth of
   Legendary Loot sets a cursor from its title screen. */
static int stand_in_cursor;
static void *(*real_create_cursor)(const void *data, const void *mask, int w, int h, int x,
                                   int y);
static void *(*real_create_color_cursor)(void *surface, int x, int y);
static void *(*real_create_system_cursor)(int id);

static void *create_cursor(const void *data, const void *mask, int w, int h, int x, int y)
{
    void *cursor = real_create_cursor(data, mask, w, h, x, y);
    return cursor ? cursor : &stand_in_cursor;
}

static void *create_color_cursor(void *surface, int x, int y)
{
    void *cursor = real_create_color_cursor(surface, x, y);
    return cursor ? cursor : &stand_in_cursor;
}

static void *create_system_cursor(int id)
{
    void *cursor = real_create_system_cursor(id);
    return cursor ? cursor : &stand_in_cursor;
}

/* Entries swapped the same way whether the SDL is the game's or the
   firmware's. SDL_GL_GetProcAddress and SDL_GL_CreateContext have
   their own handling. */
static const struct {
    const char *name;
    void *wrapper;
    void **real;
} WRAPPED[] = {
    {"SDL_GL_SetAttribute", (void *)set_attribute, (void **)&real_set_attribute},
    {"SDL_CreateCursor", (void *)create_cursor, (void **)&real_create_cursor},
    {"SDL_CreateColorCursor", (void *)create_color_cursor, (void **)&real_create_color_cursor},
    {"SDL_CreateSystemCursor", (void *)create_system_cursor, (void **)&real_create_system_cursor},
};

static void *wrap_entry(const char *name, void *fn)
{
    for (size_t i = 0; i < sizeof(WRAPPED) / sizeof(*WRAPPED); i++) {
        if (strcmp(name, WRAPPED[i].name) == 0) {
            *WRAPPED[i].real = fn;
            return WRAPPED[i].wrapper;
        }
    }
    return fn;
}

static struct sigaction next_segv;

static void print_frame(uintptr_t pc)
{
    Dl_info sym = {0};
    dladdr((void *)pc, &sym);
    fprintf(stderr, "  %#lx %s+%#lx (%s)\n", pc, sym.dli_sname ? sym.dli_sname : "?",
            sym.dli_saddr ? pc - (uintptr_t)sym.dli_saddr : 0, sym.dli_fname ? sym.dli_fname : "?");
}

/* Whether a frame record can be read, asked of the kernel so a bad
   chain does not fault inside the handler. */
static int readable(const uintptr_t *fp)
{
    static int probe[2] = {-1, -1};
    uintptr_t drained[2];
    if (probe[0] < 0 && pipe(probe) < 0) {
        return 0;
    }
    if ((uintptr_t)fp % sizeof(uintptr_t) || write(probe[1], fp, sizeof(drained)) != sizeof(drained)) {
        return 0;
    }
    return read(probe[0], drained, sizeof(drained)) == sizeof(drained);
}

/* Walks frame pointers; .NET's JIT keeps them where libunwind has no
   tables. */
static void on_segv(int sig, siginfo_t *info, void *context)
{
    const mcontext_t *regs = &((ucontext_t *)context)->uc_mcontext;
    fprintf(stderr, "sdl-dynapi: SIGSEGV reading %p, x0 %#llx\n", info->si_addr, regs->regs[0]);
    print_frame(regs->pc);
    const uintptr_t *fp = (const uintptr_t *)regs->regs[29];
    for (int i = 0; readable(fp) && i < 32; i++) {
        print_frame(fp[1]);
        fp = (const uintptr_t *)fp[0];
    }
    if (next_segv.sa_flags & SA_SIGINFO) {
        next_segv.sa_sigaction(sig, info, context);
    } else if (next_segv.sa_handler != SIG_DFL && next_segv.sa_handler != SIG_IGN) {
        next_segv.sa_handler(sig);
    } else {
        sigaction(SIGSEGV, &next_segv, NULL);
    }
}

/* ZITCH_SDL_TRACE: every GL lookup on stderr, and a backtrace on a crash. */
static int tracing(void)
{
    static int known = -1;
    if (known < 0) {
        known = getenv("ZITCH_SDL_TRACE") != NULL;
        if (known) {
            struct sigaction act = {.sa_sigaction = on_segv, .sa_flags = SA_SIGINFO | SA_NODEFER};
            sigaction(SIGSEGV, &act, &next_segv);
        }
    }
    return known;
}

static void *get_proc(const char *name)
{
    void *fn = real_get_proc(name);
    if (desktop_gl_game()) {
        fn = desktop_gl_proc(name, fn);
    }
    if (tracing()) {
        fprintf(stderr, "sdl-dynapi: GL %s -> %p\n", name, fn);
    }
    if (!fn) {
        return NULL;
    }
    if (strcmp(name, "glShaderSource") == 0) {
        real_shader_source = fn;
        return (void *)shader_source;
    }
    if (strcmp(name, "glLinkProgram") == 0) {
        real_link_program = fn;
        return (void *)link_program;
    }
    if (strcmp(name, "glDeleteShader") == 0) {
        real_delete_shader = fn;
        return (void *)delete_shader;
    }
    return fn;
}

static void free_names(const char **names, uint32_t count)
{
    for (uint32_t i = 0; i < count; i++) {
        free((char *)names[i]);
    }
    free(names);
}

/* A game with no SDL2 of its own (a .NET one) calls the firmware
   library directly, and that library asks to be overridden on its first
   call. Its own exported entry fills the table it passed, which is the
   one its stubs read; GL loading is then routed through this shim by
   swapping one slot, found the same way as a game's. */
static int32_t hook_firmware_sdl(const struct sdl_image *image, uint32_t apiver, void *table,
                                 uint32_t tablesize)
{
    void *lib = dlopen(image->path, RTLD_NOW | RTLD_NOLOAD);
    if (!lib) {
        return -1;
    }
    int32_t (*own_entry)(uint32_t, void *, uint32_t) = dlsym(lib, "SDL_DYNAPI_entry");
    void *get_proc_stub = dlsym(lib, "SDL_GL_GetProcAddress");
    if (!own_entry || own_entry(apiver, table, tablesize) < 0) {
        return -1;
    }
    uintptr_t slot = get_proc_stub ? stub_slot((uintptr_t)get_proc_stub) : 0;
    if (slot < (uintptr_t)table || slot >= (uintptr_t)table + tablesize) {
        fprintf(stderr, "sdl-dynapi: no SDL_GL_GetProcAddress slot in %s\n", image->path);
        return 0;
    }
    void **entry = (void **)slot;
    real_get_proc = *entry;
    *entry = (void *)get_proc;
    void *create_stub = dlsym(lib, "SDL_GL_CreateContext");
    slot = create_stub ? stub_slot((uintptr_t)create_stub) : 0;
    if (slot >= (uintptr_t)table && slot < (uintptr_t)table + tablesize) {
        entry = (void **)slot;
        real_create_context = *entry;
        *entry = (void *)create_context;
        real_get_drawable_size = dlsym(lib, "SDL_GL_GetDrawableSize");
    }
    for (size_t i = 0; i < sizeof(WRAPPED) / sizeof(*WRAPPED); i++) {
        void *stub = dlsym(lib, WRAPPED[i].name);
        slot = stub ? stub_slot((uintptr_t)stub) : 0;
        if (slot >= (uintptr_t)table && slot < (uintptr_t)table + tablesize) {
            entry = (void **)slot;
            *entry = wrap_entry(WRAPPED[i].name, *entry);
        }
    }
    fprintf(stderr, "sdl-dynapi: GL loading of %s routed through the shim\n", image->path);
    return 0;
}

int32_t SDL_DYNAPI_entry(uint32_t apiver, void *table, uint32_t tablesize)
{
    if (apiver != 1) {
        return -1;
    }
    struct sdl_image image = {.table = (uintptr_t)table};
    if (!dl_iterate_phdr(find_image, &image)) {
        return -1;
    }
    if (is_firmware_sdl(image.path)) {
        return hook_firmware_sdl(&image, apiver, table, tablesize);
    }

    uint32_t count = tablesize / sizeof(void *);
    const char **names = calloc(count, sizeof(char *));
    if (!names) {
        return -1;
    }
    int stubs = read_stubs(&image, count, names);
    if (stubs < 0) {
        free_names(names, count);
        return -1;
    }

    /* The firmware library reads the same variable on its first call and
       would come asking to be overridden too. */
    unsetenv("SDL_DYNAMIC_API");
    const char *path;
    void *lib = open_firmware_sdl(&path);
    if (!lib) {
        fprintf(stderr, "sdl-dynapi: no firmware SDL2: %s\n", dlerror());
        free_names(names, count);
        return -1;
    }
    void **slots = table;
    uint32_t mapped = 0;
    for (uint32_t i = 0; i < count; i++) {
        const char *name = names[i] ? names[i] : i < UPSTREAM_COUNT ? UPSTREAM[i] : NULL;
        void *fn = name ? dlsym(lib, name) : NULL;
        if (fn) {
            mapped++;
        } else if (name) {
            fprintf(stderr, "sdl-dynapi: %s has no %s\n", path, name);
        }
        if (fn && strcmp(name, "SDL_GL_GetProcAddress") == 0) {
            /* The firmware's own stub, which reads the slot get_proc
               may already sit in. */
            if (!real_get_proc) {
                real_get_proc = fn;
            }
            fn = (void *)get_proc;
        }
        if (fn) {
            fn = wrap_entry(name, fn);
        }
        if (fn && strcmp(name, "SDL_GL_CreateContext") == 0) {
            real_create_context = fn;
            real_get_drawable_size = dlsym(lib, "SDL_GL_GetDrawableSize");
            fn = (void *)create_context;
        }
        slots[i] = fn ? fn : (void *)unknown_entry;
    }
    free_names(names, count);
    fprintf(stderr, "sdl-dynapi: %u of %u SDL entries of %s routed to %s, %d read from stubs\n",
            mapped, count, image.path[0] ? image.path : "the executable", path, stubs);
    return 0;
}
