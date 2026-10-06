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

static void *(*real_get_proc)(const char *name);
static void (*real_shader_source)(unsigned shader, int count, const char *const *strings,
                                  const int *lengths);

#define DRAW_BUFFERS "#extension GL_EXT_draw_buffers : require\n"

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

/* GLSL ES 1.00 has one color target unless the shader asks for the
   draw buffers extension. Shader translators for desktop games leave
   that out, and strict drivers then refuse the shader. */
static void shader_source(unsigned shader, int count, const char *const *strings,
                          const int *lengths)
{
    size_t total = 0;
    for (int i = 0; i < count; i++) {
        total += lengths && lengths[i] >= 0 ? (size_t)lengths[i] : strlen(strings[i]);
    }
    char *source = malloc(total + sizeof(DRAW_BUFFERS));
    if (!source) {
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
    char *line_end = strchr(source, '\n');
    if (strncmp(source, "#version 100", 12) == 0 && line_end && writes_more_targets(source)
        && !strstr(source, "GL_EXT_draw_buffers")) {
        char *rest = line_end + 1;
        memmove(rest + sizeof(DRAW_BUFFERS) - 1, rest, strlen(rest) + 1);
        memcpy(rest, DRAW_BUFFERS, sizeof(DRAW_BUFFERS) - 1);
        const char *one = source;
        real_shader_source(shader, 1, &one, NULL);
    } else {
        real_shader_source(shader, count, strings, lengths);
    }
    free(source);
}

static void *get_proc(const char *name)
{
    void *fn = real_get_proc(name);
    if (fn && strcmp(name, "glShaderSource") == 0) {
        real_shader_source = fn;
        return (void *)shader_source;
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
