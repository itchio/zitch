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
 */
#define _GNU_SOURCE
#include <dlfcn.h>
#include <elf.h>
#include <fcntl.h>
#include <link.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/stat.h>
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
   with a few register moves in between. Returns the address the ldr
   reads, the table slot, or 0 for code of another shape. */
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

static void *get_proc(const char *name)
{
    void *fn = real_get_proc(name);
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

int32_t SDL_DYNAPI_entry(uint32_t apiver, void *table, uint32_t tablesize)
{
    if (apiver != 1) {
        return -1;
    }
    struct sdl_image image = {.table = (uintptr_t)table};
    if (!dl_iterate_phdr(find_image, &image) || is_firmware_sdl(image.path)) {
        return -1;
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
            real_get_proc = fn;
            fn = (void *)get_proc;
        }
        slots[i] = fn ? fn : (void *)unknown_entry;
    }
    free_names(names, count);
    fprintf(stderr, "sdl-dynapi: %u of %u SDL entries of %s routed to %s, %d read from stubs\n",
            mapped, count, image.path[0] ? image.path : "the executable", path, stubs);
    return 0;
}
